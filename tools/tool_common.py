# SPDX-License-Identifier: MIT OR Apache-2.0
"""What several tools share. A tool imports it as it imports serial_expect:
`sys.dont_write_bytecode = True`, its own folder on `sys.path`, then the
import. tools/ci-scope and tools/kani-mutation do not: they run before
anything else and stand alone."""

import importlib.machinery
import importlib.util
import os

HERE = os.path.dirname(os.path.abspath(__file__))


def load_tool(name):
    """Another tool in this directory, an extensionless script, as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module
