from pathlib import Path

# Adjust to wherever the drive is actually mounted on this machine.
LIBRARY_ROOT = Path("/mnt/game-library")

# Converted .nsp files land here, keyed by game id, so a title is only
# ever decompressed once no matter how many times it's downloaded.
# The source library itself is never written to.
#
# Defaults to a path under the current user's home directory rather
# than /var/lib/ specifically so `python server.py` works unprivileged,
# which is how this has been tested throughout. If you deploy this via
# vault-server.service instead, that unit's `StateDirectory=` directive
# creates and owns /var/lib/vault-server for you automatically — in
# that case, point this at /var/lib/vault-server/cache instead and you
# won't need sudo for that either, since systemd sets it up ahead of
# the service ever running.
CACHE_DIR = Path.home() / ".cache" / "vault-server"

# Cloud saves live here, one directory per game, each holding a handful
# of timestamped archives. Deliberately NOT under CACHE_DIR: everything
# in there can be regenerated from the library by decompressing again,
# whereas a save is the only copy of someone's progress. Losing the
# cache costs CPU; losing this costs the playthrough. Point it at
# something you actually back up.
SAVE_ROOT = Path.home() / ".local" / "share" / "vault-server" / "saves"

# How many versions of a game's save to keep before the oldest is
# pruned. The point of keeping more than one is recovering from a bad
# sync — an overwrite by a stale device, a crash mid-write — which is
# only useful if you notice within a few sessions.
SAVE_VERSIONS_KEPT = 10

HOST = "0.0.0.0"
PORT = 8420
