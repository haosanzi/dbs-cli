#/bin/bash

#let dbs-cli=/home/anuser/code/dbs-cli/target/debug/bds_cli
rm -rf /tmp/vsock.sock

/root/data/xutong/dbs-cli/target/debug/dbs-cli \
  --log-file dbs-cli.log --log-level DEBUG \
  --serial-path /tmp/dbs   \
  --kernel-path /root/data/xutong/image/bzImage \
  --firmware-path /root/data/xutong/image/final-boot-kernel.bin  \
  --rootfs /root/data/xutong/image/rootfs.img.alpine \
  --boot-args "root=/dev/sda1 tdx_disable_filter  systemd.unit=kata-containers.target agent.log=debug agent.log_vport=1025" \
  --vsock /tmp/vsock.sock create ;
