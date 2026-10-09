    .globl _start
    .text
_start:
    mov 0, %eax
    mov $60, %eax
    xor %edi, %edi
    syscall
