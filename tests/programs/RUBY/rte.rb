# RUBY has an exception_marker ("main.rb:"), so an uncaught exception is RTE; a signal is RTE too.
Process.kill("ABRT", Process.pid)
