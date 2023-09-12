#!/bin/bash

this_dir=$(dirname $(realpath $0))
root_dir=$(realpath $this_dir/..)

rm -f $this_dir/tmp.vsock.sock
rm -f $this_dir/tmp.serial.sock

# 无法使 cargo 安静, 因此把输出定向到文件
cargo build --color always 2>$this_dir/build.log
# build 失败时打印
if [ $? -ne 0 ]; then
    cat $this_dir/build.log
    exit 0
fi

$root_dir/target/debug/dbs-cli                  \
    --mem-size 2048                             \
    --log-file $this_dir/dbs-cli.log            \
    --log-level trace                           \
    --kernel-path $this_dir/bzImage             \
    --firmware-path $this_dir/final-wzy-int3.bin\
    --rootfs $this_dir/rootfs.alpine            \
    --boot-args "console=ttyS0 root=/dev/vda1"  \
    --vcpu 1                                    \
    --vsock $this_dir/tmp.vsock.sock            \
    create

    # --boot-args "rootflags=data=ordered,errors=remount-ro ro rootfstype=ext4 console=ttyS0 nokaslr debug earlyprintk=ttyS0 root=/dev/vda1" \
    # --max-vcpu 10                               \
    # --vcpu 10                                   \
    # --serial-path $this_dir/tmp.serial.sock     \
