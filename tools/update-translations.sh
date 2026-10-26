#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavick
# SPDX-License-Identifier: MIT

set -euo pipefail
cd "$(dirname "$0")/.."
python3 tools/extract-translations.py
