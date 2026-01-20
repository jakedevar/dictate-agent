#!/bin/bash
# Start dictate-agent daemon
cd ~/dictate_agent
source .venv/bin/activate  # bash script, works for exec from i3
exec .venv/bin/python -m dictate.main
