// Copyright (c) 2019-2022 Alibaba Cloud
// Copyright (c) 2019-2022 Ant Group
//
// SPDX-License-Identifier: Apache-2.0
//

use std::{
    path::{Path, PathBuf},
    sync::{
        mpsc::{Receiver, Sender},
        Arc, Mutex, RwLock,
    },
};

use crate::vmm_comm_trait::VMMComm;
use anyhow::{bail, Result};
use seccompiler::BpfProgram;
use vmm_sys_util::eventfd::EventFd;

use dragonball::{
    api::v1::{
        BlockDeviceConfigInfo, BootSourceConfig, InstanceInfo, TeeType, VmmActionError, VmmData,
        VmmRequest, VmmResponse, VsockDeviceConfigInfo,
    },
    sev::sev::{SecretWithGpa, SevSecretsInjection},
    vm::{CpuTopology, SevStart, VmConfigInfo},
    StartMicroVmError,
};

use crate::parser::DBSArgs;

const DRAGONBALL_VERSION: &str = env!("CARGO_PKG_VERSION");

pub struct CliInstance {
    /// VMM instance info directly accessible from runtime
    pub vmm_shared_info: Arc<RwLock<InstanceInfo>>,
    pub to_vmm: Option<Sender<VmmRequest>>,
    pub from_vmm: Option<Arc<Mutex<Receiver<VmmResponse>>>>,
    pub to_vmm_fd: EventFd,
    pub seccomp: BpfProgram,
}

impl VMMComm for CliInstance {
    fn get_to_vmm(&self) -> Option<&Sender<VmmRequest>> {
        self.to_vmm.as_ref()
    }

    fn get_from_vmm(&self) -> Option<Arc<Mutex<Receiver<VmmResponse>>>> {
        self.from_vmm.clone()
    }

    fn get_to_vmm_fd(&self) -> &EventFd {
        &self.to_vmm_fd
    }
}
impl CliInstance {
    pub fn new(id: &str) -> Self {
        let mut vmm_shared_info =
            InstanceInfo::new(String::from(id), DRAGONBALL_VERSION.to_string());

        vmm_shared_info.confidential_vm_type = Some(TeeType::SEV);

        let to_vmm_fd = EventFd::new(libc::EFD_NONBLOCK)
            .unwrap_or_else(|_| panic!("Failed to create eventfd for vmm {}", id));

        CliInstance {
            vmm_shared_info: Arc::new(RwLock::new(vmm_shared_info)),
            to_vmm: None,
            from_vmm: None,
            to_vmm_fd,
            seccomp: vec![],
        }
    }

    pub fn run_vmm_server(&self, args: DBSArgs) -> Result<()> {
        use dragonball::vm::SevSecureChannel;
        use sev::{cached_chain, launch::sev::*, session::Session};

        if args.boot_args.kernel_path.is_none() || args.boot_args.rootfs_args.rootfs.is_none() {
            bail!("kernel path or rootfs path cannot be None when creating the VM");
        }

        // 使用目标机器的证书, 这里为预先缓存的
        let chain = cached_chain::get().expect(
            r#"could not find certificate chain
            export with: sevctl export --full ~/.cache/amd-sev/chain"#,
        );

        let mut policy = Policy::default();
        policy.flags.set(PolicyFlags::NO_DEBUG, true);
        // policy.flags.set(PolicyFlags::ENCRYPTED_STATE, true);
        let session = Session::try_from(policy).unwrap();
        let start = Box::new(session.start(chain).unwrap());

        // configuration
        let vm_config = VmConfigInfo {
            vcpu_count: args.create_args.vcpu,
            max_vcpu_count: args.create_args.max_vcpu,
            cpu_pm: args.create_args.cpu_pm.clone(),
            cpu_topology: CpuTopology {
                threads_per_core: args.create_args.cpu_topology.threads_per_core,
                cores_per_die: args.create_args.cpu_topology.cores_per_die,
                dies_per_socket: args.create_args.cpu_topology.dies_per_socket,
                sockets: args.create_args.cpu_topology.sockets,
            },
            vpmu_feature: 0,
            mem_type: args.create_args.mem_type.clone(),
            mem_file_path: args.create_args.mem_file_path.clone(),
            mem_size_mib: args.create_args.mem_size,
            // as in crate `dragonball` serial_path will be assigned with a default value,
            // we need a special token to enable the stdio console.
            serial_path: args.create_args.serial_path.clone(),
            // userspace_ioapic_enabled: true,
            sev_start: SevStart::new(
                true,
                start.policy,
                Some(Box::new(SevSecureChannel {
                    cert: start.cert,
                    session: start.session,
                })),
            ),
        };

        // check the existence of the serial path (rm it if exist)
        if let Some(serial_path) = &args.create_args.serial_path {
            let serial_path = Path::new(serial_path);
            if serial_path.exists() {
                std::fs::remove_file(serial_path).unwrap();
            }
        }

        // boot source
        let boot_source_config = BootSourceConfig {
            // unwrap is safe because we have checked kernel_path in the beginning of run_vmm_server
            kernel_path: args.boot_args.kernel_path.unwrap(),
            initrd_path: args.boot_args.initrd_path.clone(),
            firmware_path: args.boot_args.firmware_path.clone(),
            boot_args: Some(args.boot_args.boot_args.clone()),
        };

        // rootfs
        let mut block_device_config_info = BlockDeviceConfigInfo::default();
        block_device_config_info = BlockDeviceConfigInfo {
            drive_id: String::from("rootfs"),
            // unwrap is safe because we have checked rootfs path in the beginning of run_vmm_server
            path_on_host: PathBuf::from(&args.boot_args.rootfs_args.rootfs.unwrap()),
            is_root_device: args.boot_args.rootfs_args.is_root,
            is_read_only: args.boot_args.rootfs_args.is_read_only,
            ..block_device_config_info
        };

        // set vm configuration
        self.set_vm_configuration(vm_config)
            .expect("failed to set vm configuration");

        // set boot source config
        self.put_boot_source(boot_source_config)
            .expect("failed to set boot source");

        // set rootfs
        self.insert_block_device(block_device_config_info)
            .expect("failed to set block device");

        if !args.create_args.vsock.is_empty() {
            // VSOCK config
            let mut vsock_config_info = VsockDeviceConfigInfo::default();
            vsock_config_info = VsockDeviceConfigInfo {
                guest_cid: 42, // dummy value
                uds_path: Some(args.create_args.vsock.to_string()),
                ..vsock_config_info
            };

            // set vsock
            self.insert_vsock(vsock_config_info)
                .expect("failed to set vsock socket path");
        }

        // start sev micro-vm
        let response = self.instance_start_sev().unwrap();
        let VmmData::SevMeasurement(msr) = response else { panic!()};
        // println!("cmdline: {:?}", msr.cmdline.as_slice());
        // println!("tdhob: {:?}", msr.tdhob.as_slice());

        let session = session.verify(&[], msr.build, msr.measurement).unwrap();

        const CODE: &[u8; 16] = &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53];
        let secret = session.secret(HeaderFlags::default(), CODE).unwrap();

        self.inejct_sev_secrets(SevSecretsInjection {
            secrets: vec![SecretWithGpa { secret, gpa: None }],
            resume_vm: true,
        })
        .unwrap();

        Ok(())
    }
}
