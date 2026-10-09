{ FPC turns SIGSEGV and division by zero into "runtime error N" with exit code N, which the verdict table
  reads as IR; a signal FPC does not trap is RTE. }
uses baseunix;
begin fpkill(fpgetpid, SIGABRT); end.
