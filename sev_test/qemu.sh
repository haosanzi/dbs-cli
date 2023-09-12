#!/bin/bash

sudo /home/zongyong.wzy/qemu/build/x86_64-softmmu/qemu-system-x86_64 \
    -drive if=pflash,format=raw,unit=0,file=/home/zongyong.wzy/images/OVMF_CODE.fd,readonly=on \
    -drive format=raw,file=/home/zongyong.wzy/images/bzImage \
    -serial mon:stdio -accel kvm -smp 12 -m size=512M \
    -object '{"qom-type":"sev-guest","id":"lsec0","cbitpos":51,"reduced-phys-bits":1,"policy":1}' \
    -machine pc-q35-7.2,confidential-guest-support=lsec0 \
    -no-reboot -cpu EPYC
