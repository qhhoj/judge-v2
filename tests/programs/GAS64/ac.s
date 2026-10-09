    .globl _start
    .text
_start:
    xor %eax, %eax          # read(0, buf, 4)
    xor %edi, %edi
    lea buf(%rip), %rsi
    mov $4, %edx
    syscall
    movzbl buf(%rip), %eax
    movzbl buf+2(%rip), %ebx
    add %ebx, %eax
    sub $48, %eax           # '0' + '0' - '0'
    mov %al, out(%rip)
    mov $1, %eax            # write(1, out, 2)
    mov $1, %edi
    lea out(%rip), %rsi
    mov $2, %edx
    syscall
    mov $60, %eax           # exit(0)
    xor %edi, %edi
    syscall
    .data
buf: .space 4
out: .byte 0, 10
