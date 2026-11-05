# =============================================================================
# HYDRA-UMC-SDK - Shared lock on builds that compile on the developer's PC
# Copyright (C) 2026 JuanenRac (Electro Hobby 3D) <electrohobby3d@gmail.com>
# GPL-3.0-or-later - see LICENSE
# =============================================================================
"""The lock on builds that compile on the machine of the developer.

A C compiler, WSL, Cargo, Go, Gradle or an npm build can take a workstation to its knees. Tests and tools that need one ask this module first: they
build on a continuous integration runner (the variable ``CI`` or ``GITHUB_ACTIONS`` is set), or on a machine whose owner said so with
``ALLOW_LOCAL_BUILD=1``, and nowhere else. Anywhere else they report that the build was skipped, never that it passed.

Standard library only; the same file is kept in every repository that compiles something (`tools/sync_repo_tools.py` writes it, `--check` compares it).
"""
from __future__ import annotations

import os
from typing import Mapping

ALLOW_VARIABLE = "ALLOW_LOCAL_BUILD"
LOCK_MESSAGE = "local builds are off; GitHub Actions builds it. Set ALLOW_LOCAL_BUILD=1 to allow it on this PC"
_FALSE = {"", "0", "false", "no", "off"}


def _on(value: str | None) -> bool:
    return (value or "").strip().lower() not in _FALSE


def local_build_allowed(env: Mapping[str, str] | None = None) -> bool:
    """True on a CI runner (``CI`` or ``GITHUB_ACTIONS`` set) or when ``ALLOW_LOCAL_BUILD=1``."""
    environment = os.environ if env is None else env
    if environment.get(ALLOW_VARIABLE, "").strip() == "1":
        return True
    return _on(environment.get("CI")) or _on(environment.get("GITHUB_ACTIONS"))


def local_build_refusal(env: Mapping[str, str] | None = None) -> str | None:
    """The message to show when a build that compiles locally must not run, or None when it may."""
    return None if local_build_allowed(env) else LOCK_MESSAGE
