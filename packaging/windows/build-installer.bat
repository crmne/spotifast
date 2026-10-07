@echo off
rem Builds the x64 release binary and its installer on a Windows machine, the
rem way the release workflow does, so a setup.exe can be made without a tag:
rem
rem   packaging\windows\build-installer.bat
rem
rem Needs the Visual Studio C++ build tools, LLVM (for libclang), Inno Setup 6,
rem and vcpkg with glew:x64-windows-static installed. vcpkg is looked up in
rem VCPKG_INSTALLATION_ROOT, then %USERPROFILE%\vcpkg; LIBCLANG_PATH defaults
rem to the LLVM installer's location. The installer lands in dist\.
setlocal

set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (
  echo Visual Studio Installer not found. Install the C++ build tools.
  exit /b 1
)
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL (
  echo No Visual Studio installation with the C++ x64 tools was found.
  exit /b 1
)
rem vcvars64.bat runs vswhere itself and complains when it is not on PATH.
set "PATH=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer;%PATH%"
call "%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1

if not defined VCPKG_INSTALLATION_ROOT set "VCPKG_INSTALLATION_ROOT=%USERPROFILE%\vcpkg"
if not exist "%VCPKG_INSTALLATION_ROOT%\installed\x64-windows-static\include\GL\glew.h" (
  echo GLEW not found under %VCPKG_INSTALLATION_ROOT%. Run:
  echo   vcpkg install glew:x64-windows-static
  exit /b 1
)
if not defined LIBCLANG_PATH set "LIBCLANG_PATH=%ProgramFiles%\LLVM\bin"
if not exist "%LIBCLANG_PATH%\libclang.dll" (
  echo libclang.dll not found in %LIBCLANG_PATH%. Install LLVM or set LIBCLANG_PATH.
  exit /b 1
)
rem projectm-sys asks CMake for a Visual Studio generator; Ninja inside the
rem MSVC environment builds the same thing, as in CI.
set CMAKE_GENERATOR=Ninja

set "ISCC=%LOCALAPPDATA%\Programs\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" set "ISCC=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" (
  echo Inno Setup 6 not found. Install it, for example with:
  echo   winget install JRSoftware.InnoSetup
  exit /b 1
)

cd /d "%~dp0..\.."
cargo build --release --locked || exit /b 1

rem The installer must run on a clean machine: no MSVC runtime imports.
dumpbin /dependents target\release\spotifast.exe | findstr /i "MSVCP VCRUNTIME" >nul
if not errorlevel 1 (
  echo spotifast.exe imports an MSVC runtime DLL; check .cargo\config.toml.
  exit /b 1
)

for /f "tokens=2" %%v in ('target\release\spotifast.exe --version') do set "VERSION=%%v"
if not defined VERSION (
  echo Could not read the version from spotifast.exe --version.
  exit /b 1
)

"%ISCC%" /Q "/DVersion=%VERSION%" /DArch=x86_64 ^
  "/DBinary=%CD%\target\release\spotifast.exe" ^
  "/DOutputDir=%CD%\dist" packaging\windows\spotifast.iss || exit /b 1
echo Built dist\spotifast-v%VERSION%-x86_64-pc-windows-msvc-setup.exe
