@echo off
setlocal
pushd "%~dp0"
cargo build --release
if errorlevel 1 (
    echo.
    echo RustCmd build failed.
    popd
    exit /b 1
)
echo.
echo Built: %CD%\target\release\rustcmd.exe
popd
exit /b 0

