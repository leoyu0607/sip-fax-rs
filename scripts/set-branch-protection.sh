#!/usr/bin/env bash
# Apply main branch protection from scripts/branch-protection.json.
# Required CI checks must pass; admins may bypass (enforce_admins=false).
# Requires a GitHub token with admin rights on the repo.
set -euo pipefail
cd "$(dirname "$0")"
gh api -X PUT repos/leoyu0607/sip-fax-rs/branches/main/protection \
  --input branch-protection.json \
  -q '{checks: .required_status_checks.contexts, enforce_admins: .enforce_admins.enabled}'
