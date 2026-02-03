#!/bin/bash
# Start dictate-agent daemon
cd ~/dictate_agent
source .venv/bin/activate
exec .venv/bin/python -m dictate.main
