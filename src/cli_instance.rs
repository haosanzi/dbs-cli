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

        if args.boot_args.kernel_path.is_none() || args.boot_args.rootfs_args.rootfs.is_none() {
            bail!("kernel path or rootfs path cannot be None when creating the VM");
        }

        println!("args is {:?}", args);
        let security_info = args.security_info_args.unwrap();
        let mut sev_config = aeb::kbs::GuestPreAttestationConfig {
            proxy: security_info.guest_pre_attestation_proxy.unwrap(),
            cert_chain_path: security_info.sev_cert_chain_path.unwrap(),
            policy: security_info.sev_guest_policy,
            ..Default::default()
        };

        println!("sev_config_bundle_request is {:?}", sev_config);

        let (sev_attestation_id, start) = async_std::task::block_on(async {
            aeb::setup_sevguest_pre_attestation(&sev_config).await
        })?;

        println!("attestation id is {:?}", sev_attestation_id);

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
            kernel_path: args.boot_args.kernel_path.clone().unwrap(),
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

        let measurement = msr.measurement;
        let _build = msr.build;
        let cmdline = msr.cmdline;
        let tdhob = msr.tdhob;

        sev_config.keyset = security_info.guest_pre_attestation_keyset.unwrap();
        sev_config.launch_id = sev_attestation_id;
        sev_config.firmware = args.boot_args.firmware_path;
        sev_config.kernel = args.boot_args.kernel_path;
        sev_config.initrd = args.boot_args.initrd_path;
        sev_config.cmdline = cmdline;
        sev_config.tdhob = tdhob;
        sev_config.key_broker_secret_guid =
            security_info.guest_pre_attestation_secret_guid.unwrap();
        sev_config.key_broker_secret_type =
            security_info.guest_pre_attestation_secret_type.unwrap();
        sev_config.num_vcpu = args.create_args.vcpu;

        println!("sev_config_secret_request is {:?}", sev_config);
        sev_config.confidential_vm_type = "sev".to_string();

        let secret = async_std::task::block_on(async {
            aeb::sev_guest_pre_attestation(&sev_config, measurement).await
        })?;

        println!("secret is {:?}", secret);

        self.inejct_sev_secrets(SevSecretsInjection {
            secrets: vec![SecretWithGpa { secret, gpa: None }],
            resume_vm: true,
        })
        .unwrap();

        Ok(())
    }
}
