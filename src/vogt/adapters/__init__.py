"""Adapters: thin translations between a transport and the application layer.

An adapter may parse, authenticate, and render. It may not decide anything —
every decision belongs to the application layer, which is what makes the
three surfaces agree by construction rather than by discipline.
"""

from __future__ import annotations
