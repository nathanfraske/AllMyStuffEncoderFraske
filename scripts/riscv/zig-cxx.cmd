@echo off
"%ALLMYSTUFF_RISCV_ZIG%" c++ %* --target=riscv64-linux-musl -mcpu=generic_rv64+m+a+f+d+c+zicsr+zifencei -mabi=lp64d
exit /b %errorlevel%
