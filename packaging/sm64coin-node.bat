@echo off
rem sm64coin-node.bat: run an SM64Coin node (sm64coind.exe) without the wallet app.
rem Usage:  sm64coin-node.bat [--solo] [--data-dir DIR] [--rom FILE] [--public-rpc] [-- extra sm64coind args]
rem Environment: DATA_DIR, ROM, P2P_PORT, RPC_PORT, ZMQ_PORT. Open TCP 16480 in the firewall to accept peers.
rem Seeds: seeds.txt next to this file. Hostnames are resolved with PowerShell (Resolve-DnsName) to IPv4,
rem and skipped with a message when they do not resolve. The ROM's SHA-1 is checked with certutil.
setlocal EnableDelayedExpansion
set "HERE=%~dp0"
if "%DATA_DIR%"=="" set "DATA_DIR=%USERPROFILE%\.sm64coin\node"
if "%P2P_PORT%"=="" set "P2P_PORT=16480"
if "%RPC_PORT%"=="" set "RPC_PORT=16481"
if "%ZMQ_PORT%"=="" set "ZMQ_PORT=16482"
set "SOLOARG="
set "PUBARG="
set "EXTRA="
:args
if "%~1"=="" goto argsdone
if /i "%~1"=="--solo" (set "SOLOARG=--sm64-solo" & shift & goto args)
if /i "%~1"=="--public-rpc" (set "PUBARG=--rpc-restricted-bind-ip 0.0.0.0 --rpc-restricted-bind-port 16489 --confirm-external-bind" & shift & goto args)
if /i "%~1"=="--data-dir" (set "DATA_DIR=%~2" & shift & shift & goto args)
if /i "%~1"=="--rom" (set "ROM=%~2" & shift & shift & goto args)
if /i "%~1"=="--" (shift & goto extra)
if /i "%~1"=="--help" goto help
if /i "%~1"=="-h" goto help
echo sm64coin-node: unknown option %~1 ^(--help^)
exit /b 2
:extra
if "%~1"=="" goto argsdone
set "EXTRA=!EXTRA! %1"
shift
goto extra
:help
echo Usage: sm64coin-node.bat [--solo] [--data-dir DIR] [--rom FILE] [--public-rpc] [-- extra sm64coind args]
echo Environment: DATA_DIR ROM P2P_PORT RPC_PORT ZMQ_PORT. Needs the US ROM baserom.us.z64 ^(SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce^).
exit /b 0
:argsdone

if not exist "%HERE%sm64coind.exe" (echo sm64coin-node: sm64coind.exe not found next to this file & exit /b 2)

rem ---- ROM: --rom / ROM, else next to this file, else in %USERPROFILE%\.sm64coin ----
set "ROMFILE="
if not "%ROM%"=="" (
  if exist "%ROM%" (set "ROMFILE=%ROM%") else (echo sm64coin-node: the ROM path %ROM% does not exist & exit /b 2)
)
if "%ROMFILE%"=="" if exist "%HERE%baserom.us.z64" set "ROMFILE=%HERE%baserom.us.z64"
if "%ROMFILE%"=="" if exist "%USERPROFILE%\.sm64coin\baserom.us.z64" set "ROMFILE=%USERPROFILE%\.sm64coin\baserom.us.z64"
if "%ROMFILE%"=="" (
  echo sm64coin-node: Could not find your Super Mario 64 ROM.
  echo.
  echo SM64Coin needs the US version of Super Mario 64 as a .z64 file ^(SHA-1 9bef1128717f958171a4afac3ed78ee2bb4e86ce^).
  echo It is not included: you have to supply your own copy. Do one of these and run again:
  echo   - copy it next to this file, named baserom.us.z64  ^(%HERE%^)
  echo   - or copy it to %USERPROFILE%\.sm64coin\baserom.us.z64
  echo   - or give its path:  sm64coin-node.bat --rom C:\path\to\rom.z64
  exit /b 2
)
set "SHA="
for /f "skip=1 delims=" %%h in ('certutil -hashfile "%ROMFILE%" SHA1') do if not defined SHA set "SHA=%%h"
set "SHA=!SHA: =!"
if /i not "!SHA!"=="9bef1128717f958171a4afac3ed78ee2bb4e86ce" (
  echo sm64coin-node: %ROMFILE% is not the US ROM ^(its SHA-1 is !SHA!, expected 9bef1128717f958171a4afac3ed78ee2bb4e86ce^).
  exit /b 2
)

rem ---- seeds: resolve each host to IPv4 with PowerShell ----
set "PEERARGS="
if exist "%HERE%seeds.txt" (
  for /f "usebackq tokens=* delims=" %%L in ("%HERE%seeds.txt") do (
    set "line=%%L"
    for /f "tokens=1 delims=#" %%A in ("!line!") do (
      set "ent=%%A"
      set "ent=!ent: =!"
      if not "!ent!"=="" call :addpeer !ent!
    )
  )
)
if not exist "%DATA_DIR%" mkdir "%DATA_DIR%"
echo sm64coin-node: ROM %ROMFILE%, data %DATA_DIR%, p2p %P2P_PORT% rpc %RPC_PORT% zmq %ZMQ_PORT%
"%HERE%sm64coind.exe" --data-dir "%DATA_DIR%" --sm64-rom "%ROMFILE%" --p2p-bind-port %P2P_PORT% --rpc-bind-port %RPC_PORT% --zmq-rpc-bind-port %ZMQ_PORT% --max-connections-per-ip=16 --rpc-access-control-origins https://gumbo64.github.io --non-interactive --log-file "%DATA_DIR%\sm64coind.log" %PEERARGS% %SOLOARG% %PUBARG% %EXTRA%
exit /b %ERRORLEVEL%

:addpeer
set "ent=%~1"
set "host=%ent%"
set "port=16480"
for /f "tokens=1,2 delims=:" %%a in ("%ent%") do (set "host=%%a" & if not "%%b"=="" set "port=%%b")
set "ip="
echo %host%| findstr /r "^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$" >nul && set "ip=%host%"
if not defined ip for /f "delims=" %%i in ('powershell -NoProfile -Command "try { (Resolve-DnsName -Type A -Name '%host%' -ErrorAction Stop | Where-Object { $_.Type -eq 'A' } | Select-Object -First 1).IPAddress } catch { }"') do set "ip=%%i"
if not defined ip (echo sm64coin-node: seed %host% does not resolve to an IPv4 address: skipped & exit /b 0)
set "PEERARGS=!PEERARGS! --add-peer %ip%:%port% --add-priority-node %ip%:%port%"
exit /b 0
