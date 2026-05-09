"""Runtime plugin catalog.

Each module in this package exposes either a module-level ``plugin``
attribute (for zero-config plugins) or a class importable via
``module:Class`` (when the plugin requires a factory). They are loaded
through :func:`circuitry.core.runtime_plugins.load_plugins`.
"""
