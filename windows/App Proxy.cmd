@echo off
chcp 65001 >nul
cd /d "%~dp0"
if exist "%~dp0runtime\node.exe" (
  "%~dp0runtime\node.exe" "%~dp0src\cli.ts"
) else (
  where node >nul 2>nul
  if errorlevel 1 (
    echo Node.js 24 or newer is required. Use the portable release package.
    pause
    exit /b 1
  )
  node "%~dp0src\cli.ts"
)
if errorlevel 1 pause
