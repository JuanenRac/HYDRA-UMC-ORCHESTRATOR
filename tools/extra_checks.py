# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/extra_checks.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""The checks of this repository beyond the shared ones: the two pictures of the README and the file tree.

`docs/architecture.json` draws `images/architecture.svg` and `docs/structure.json` draws `images/repository-structure.svg` and the text tree of the eight
READMEs; both checks fail when the picture, the JSON or the code it points to disagree (see docs/README_PICTURES.md of the A.R.M.O.R. common repository for the tools).
"""
from __future__ import annotations

import subprocess
import sys
from pathlib import Path

CHECKS = (
    ("make_architecture.py", ["--check"]),
    ("make_structure.py", ["--check", "--readmes"]),
)


def check(root: Path) -> list[str]:
    problems: list[str] = []
    for script, arguments in CHECKS:
        path = root / "tools" / script
        if not path.is_file() or not (root / "docs" / ("architecture.json" if "architecture" in script else "structure.json")).is_file():
            continue
        result = subprocess.run([sys.executable, "-I", str(path), *arguments],
                                cwd=root, text=True, encoding="utf-8", errors="replace", capture_output=True, check=False)
        if result.returncode != 0:
            problems.append(f"tools/{script} {' '.join(arguments)}: " + (result.stdout + result.stderr).strip().splitlines()[-1][:300])
    return problems
