@echo off
setlocal
echo Building am2strata-desktop (Release)...
cargo build --release --manifest-path "G:\Strata-3060\am2strata-desktop\Cargo.toml"
if errorlevel 1 (
    echo Build failed!
    pause
    exit /b 1
)
copy /Y "G:\Strata-3060\am2strata-desktop\target\release\am2strata-desktop.exe" "G:\Strata-3060\am2strata\am2strata-desktop.exe" >nul
echo Done! Installed G:\Strata-3060\am2strata\am2strata-desktop.exe
endlocal
