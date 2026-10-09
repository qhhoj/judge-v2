    .globl _start
    .text
_start:
    mov $3, %eax            # read(0, buf, 4)
    xor %ebx, %ebx
    mov $buf, %ecx
    mov $4, %edx
    int $0x80
    movzbl buf, %eax
    movzbl buf+2, %ebx
    add %ebx, %eax
    sub $48, %eax
    mov %al, out
    mov $4, %eax            # write(1, out, 2)
    mov $1, %ebx
    mov $out, %ecx
    mov $2, %edx
    int $0x80
    mov $1, %eax            # exit(0)
    xor %ebx, %ebx
    int $0x80
    .data
buf: .space 4
out: .byte 0, 10
