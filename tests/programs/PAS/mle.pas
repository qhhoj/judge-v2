var p: pointer;
begin while true do begin getmem(p, 1048576); fillchar(p^, 1048576, 1); end; end.
