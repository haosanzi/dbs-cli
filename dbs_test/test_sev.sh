#/bin/bash

this_dir=$(dirname $(realpath $0))
root_dir=$(realpath $this_dir/..)

rm $this_dir/tmp.vsock.sock
# rm $this_dir/tmp.serial.sock

$root_dir/target/debug/dbs-cli \
  --log-file $this_dir/dbs-cli.log \
  --log-level debug \
  --serial-path $this_dir/tmp.serial.sock \
  --kernel-path $this_dir/bzImage \
  --firmware-path $this_dir/final-boot-kernel.bin \
  --rootfs $this_dir/rootfs.alpine \
  --boot-args "root=/dev/sda1 systemd.unit=kata-containers.target agent.log=debug agent.log_vport=1025" \
  --vsock $this_dir/tmp.vsock.sock \
  create
