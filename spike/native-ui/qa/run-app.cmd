@echo off
set EGUI_INSPECTION=1
set EGUI_INSPECTION_ADDR=127.0.0.1:5719
set /p RIPDL_QA_SCREEN=<"%~dp0screen.txt"
"%~dp0..\target\debug\ripdl-native-ui-spike.exe"
