@echo off
"%ALLMYSTUFF_RISCV_ZIG%" ar s %*
exit /b %errorlevel%
