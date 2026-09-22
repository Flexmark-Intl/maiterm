#!/bin/sh
# Register .maiterm-workspace (see linux/maiterm-workspace.xml). Failure here must not fail the install.
if command -v update-mime-database >/dev/null 2>&1; then
  update-mime-database /usr/share/mime >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi
exit 0
