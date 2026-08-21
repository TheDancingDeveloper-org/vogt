"""Encrypt per-actor forge PATs at rest (#179, design #178 decision 4).

A linked Personal Access Token is a fundamentally different category from the
vogt-issued tokens in `0005_tokens`. Those are stored as a *hash* because they
never need recovery — a lost one is rotated, not looked up. A forge PAT is the
opposite: Vogt has to hand it back to the upstream API on the actor's behalf,
so it must be *recoverable*, which means encrypted rather than hashed and never
stored in plaintext.

The key lives in a file named by `forge_account_key_file`. If that is unset or
unreadable the feature is *off*: `load_cipher` raises the typed
`ForgeAccountsNotConfigured`, which the link operation turns into an honest
"linking is not configured" answer rather than pretending to store a secret it
has nowhere safe to put.
"""

from __future__ import annotations

from dataclasses import dataclass

from cryptography.fernet import Fernet, InvalidToken

from vogt.config import VogtConfig
from vogt.errors import ForgeAccountsNotConfigured


@dataclass(frozen=True)
class ForgeAccountCipher:
    """Fernet encrypt/decrypt for one instance's account-linking key."""

    _fernet: Fernet

    def encrypt(self, plaintext: str) -> str:
        """The PAT as Fernet ciphertext — the only form that is ever stored."""
        return self._fernet.encrypt(plaintext.encode("utf-8")).decode("utf-8")

    def decrypt(self, ciphertext: str) -> str:
        """Recover the PAT to call upstream. Raises on a key that cannot open it."""
        try:
            return self._fernet.decrypt(ciphertext.encode("utf-8")).decode("utf-8")
        except InvalidToken as exc:
            msg = (
                "the stored forge token could not be decrypted with the "
                "configured key; the key may have been rotated or replaced"
            )
            raise ForgeAccountsNotConfigured(msg) from exc


def load_cipher(config: VogtConfig) -> ForgeAccountCipher:
    """Build the cipher, or refuse honestly when linking is not configured.

    Every failure to obtain a usable key is the same answer to the caller —
    the feature is off — so a missing path, a missing file and a malformed key
    all raise the one typed error rather than three shades of the same "no".
    """
    path = config.forge_account_key_file
    if path is None:
        msg = (
            "forge account linking is not configured (no key file); set "
            "forge_account_key_file to a file holding a Fernet key to enable it"
        )
        raise ForgeAccountsNotConfigured(msg)
    resolved = path.expanduser()
    if not resolved.is_file():
        msg = (
            f"forge account linking is not configured: the key file {resolved} "
            "does not exist, so there is nowhere safe to store a token"
        )
        raise ForgeAccountsNotConfigured(msg)
    key = resolved.read_bytes().strip()
    try:
        return ForgeAccountCipher(Fernet(key))
    except (ValueError, TypeError) as exc:
        msg = (
            "forge account linking is not configured: the key file does not "
            "hold a valid urlsafe-base64 Fernet key"
        )
        raise ForgeAccountsNotConfigured(msg) from exc


def account_linking_enabled(config: VogtConfig) -> bool:
    """Whether a usable key is present, without raising.

    The write path asks this before it even looks for a linked PAT: with no
    key there is nothing to decrypt, so the file-token fallback is the whole
    answer and no read of the accounts table is needed.
    """
    try:
        load_cipher(config)
    except ForgeAccountsNotConfigured:
        return False
    return True


__all__ = ["ForgeAccountCipher", "account_linking_enabled", "load_cipher"]
