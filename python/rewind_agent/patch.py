"""
Monkey-patching layer for OpenAI clients.

When `init()` is called, it patches the OpenAI client to route
all API calls through the local Rewind proxy, which records them.
"""

import os
import contextlib
from functools import wraps

_original_base_url = None
_initialized = False

REWIND_PROXY_URL = "http://127.0.0.1:8443"


def init(proxy_url: str | None = None, auto_patch: bool = True):
    """
    Initialize Rewind recording.

    This patches the OPENAI_BASE_URL environment variable so that
    all OpenAI client instances route through the Rewind proxy.

    Args:
        proxy_url: Override the default proxy URL (http://127.0.0.1:8443)
        auto_patch: If True, also monkey-patch already-imported OpenAI clients
    """
    global _original_base_url, _initialized

    if _initialized:
        return

    url = proxy_url or REWIND_PROXY_URL

    # Save original
    _original_base_url = os.environ.get("OPENAI_BASE_URL")

    # Set proxy URL — new OpenAI() clients will pick this up automatically
    os.environ["OPENAI_BASE_URL"] = f"{url}/v1"

    # Also patch Anthropic if available
    os.environ["ANTHROPIC_BASE_URL"] = f"{url}/anthropic"

    _initialized = True

    if auto_patch:
        _patch_existing_clients(url)

    _print_banner(url)


def uninit():
    """Restore original base URLs and remove patches."""
    global _original_base_url, _initialized

    if not _initialized:
        return

    if _original_base_url is not None:
        os.environ["OPENAI_BASE_URL"] = _original_base_url
    else:
        os.environ.pop("OPENAI_BASE_URL", None)

    os.environ.pop("ANTHROPIC_BASE_URL", None)
    _initialized = False


@contextlib.contextmanager
def session(name: str = "default", proxy_url: str | None = None):
    """
    Context manager for a Rewind recording session.

    Usage:
        with rewind_agent.session("my-agent"):
            client = openai.OpenAI()
            client.chat.completions.create(...)
    """
    # TODO: Start a named session via the proxy API
    init(proxy_url=proxy_url)
    try:
        yield
    finally:
        uninit()


def _patch_existing_clients(proxy_url: str):
    """Patch already-instantiated OpenAI clients if the module is loaded."""
    try:
        import openai
        # Patch the module-level default client if it exists
        if hasattr(openai, '_client'):
            openai._client.base_url = f"{proxy_url}/v1"
    except ImportError:
        pass


def _print_banner(proxy_url: str):
    """Print a nice startup banner."""
    print()
    print("  \033[36m\033[1m⏪ Rewind\033[0m — Recording active")
    print()
    print(f"  \033[90mProxy:\033[0m  {proxy_url}")
    print(f"  \033[90mOpenAI:\033[0m {proxy_url}/v1")
    print()
    print("  \033[33mAll LLM calls are being recorded.\033[0m")
    print("  Run \033[32mrewind show latest\033[0m to see the trace.")
    print()
