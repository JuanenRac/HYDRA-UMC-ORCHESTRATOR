# =============================================================================
# HYDRA-UMC-ORCHESTRATOR - tools/_doc_policy.py
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
# VENDORED - do not hand-edit. This is a byte-for-byte copy of
# HYDRA-UMC-SDK's own canonical
# clients/python/src/hydra_umc_sdk/doc_policy.py, kept in sync by
# that repo's own tools/sync_doc_policy.py. Edit the rule there, then
# re-run that script to update every repo that vendors it, this one
# included.

from __future__ import annotations

import subprocess
from pathlib import Path

# Split across string concatenation so this module's own source is never
# itself flagged by the very check it defines.
PRIVATE_MARKERS: tuple[str, ...] = ("SON" + "NET", "BIB" + "LIA")

# Prose that names this ecosystem's private planning or audit material,
# beyond the bare marker names above.
PRIVATE_PHRASES: tuple[str, ...] = (
    "BIB" + "LIA HYDRA" + "-UMC",
    "private development" + " plan",
    "plan de desarrollo" + " privado",
    "internal work" + " log",
    "registro de trabajo" + " interno",
)


def check_public_private_boundary(root: Path) -> str | None:
    """Runs this ecosystem's real, two-part public/private documentation
    boundary check against `root`'s own git-tracked files: no PRIVATE_MARKERS
    name anywhere, and no PRIVATE_PHRASES prose anywhere. Returns a real,
    ready-to-`fail()` error message on the first violation or check failure,
    or `None` if the boundary genuinely holds.

    `root` must be a real git working tree (this shells out to `git grep`
    there) - the same boundary this repo and every sibling repo's own CI
    already enforced individually before this module existed.
    """
    for marker in PRIVATE_MARKERS:
        result = subprocess.run(
            ("git", "grep", "-n", "-I", "--", marker),
            cwd=root,
            text=True,
            encoding="utf-8",
            errors="replace",
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        if result.returncode == 0:
            return "public files must not reference private documentation"
        if result.returncode not in (0, 1):
            return "could not check public/private documentation boundary"

    phrase_cmd = ["git", "grep", "-n", "-I", "-i", "-F"]
    for phrase in PRIVATE_PHRASES:
        phrase_cmd += ["-e", phrase]
    prose_result = subprocess.run(
        tuple(phrase_cmd),
        cwd=root,
        text=True,
        encoding="utf-8",
        errors="replace",
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if prose_result.returncode == 0:
        return "public files must not reference private planning or audit documents"
    if prose_result.returncode not in (0, 1):
        return "could not check public/private documentation boundary"

    return None
