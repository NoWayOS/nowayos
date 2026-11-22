# SPDX-License-Identifier: MIT OR Apache-2.0
"""What the tools' tests share: loading a tool, an extensionless script in
this directory, as a module; and the environment that keeps a scratch git
repository's commits free of the machine's own configuration."""

import importlib.machinery
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))

# Git configuration and identity for the scratch repositories the tests make.
GIT_ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
}


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module
