# Private Tor test network fixtures

This directory holds **test-only** directory-authority (DA) identities for
the private, fully-isolated Tor network used by the issue #5 NixOS VM test
(see `nix/tor-test-net/torrc.nix`, consumed by the NixOS test framework).
It is a Nix port of the `dax_dev` "testing Tor network" tutorial
(https://gitlab.torproject.org/tpo/core/chutney and the historical
`doc/HACKING/HowToReleaseIndependentTorVersion` DA-cert docs describe the
same `tor-gencert` workflow).

## Why keys are committed

`services.tor.settings.DirAuthority` lines must embed each DA's v3 identity
fingerprint and relay fingerprint as plain strings *at Nix evaluation time*
(see `torrc.nix`'s `common.DirAuthority`). There is no way to generate them
at VM boot and still have the other DAs' `torrc`s reference them, short of
a multi-phase bootstrap the NixOS test framework doesn't support here. So
the fixture keys are pre-generated once, checked into git, and every VM
in the private network boots with the same fixed identities.

**These keys have no security value.** They exist only to give the private
test network stable, well-known fingerprints. Never reuse them for
anything that touches the real Tor network.

**Deviation from a normal deployment:** the VM test points arti's fallback
caches at the three DAs (which double as relays/dir caches on this network)
rather than at the relays. On this private network the two sets of nodes
are functionally equivalent, so this is not expected to affect coverage.

## Layout

Each `da{1,2,3}/` directory is a trimmed-down Tor `DataDirectory`:

- `keys/authority_identity_key`, `keys/authority_signing_key`,
  `keys/authority_certificate` — the v3 DA identity/signing keypair and
  the certificate binding them (generated with `tor-gencert`).
- `keys/secret_id_key`, `keys/ed25519_master_id_secret_key`,
  `keys/ed25519_master_id_public_key` — the relay identity keys that back
  the DA's own onion-router fingerprint (the trailing fingerprint in its
  `DirAuthority` line).
- `fingerprint`, `fingerprint-ed25519` — the resulting relay
  fingerprints, as written by `tor --list-fingerprint`.

Everything else Tor writes at runtime (lock files, caches, onion/signing
subkeys that Tor regenerates automatically from the master identity key,
state files) is deliberately **not** committed.

The real fingerprints extracted from these files live in
`fingerprints.nix`, consumed by `torrc.nix`.

## Regenerating

Requires the `tor` package (ships both `tor` and `tor-gencert`):

```bash
nix --extra-experimental-features 'nix-command flakes' shell nixpkgs#tor
```

Then, for each `i` in `1 2 3`:

```bash
d=nix/tor-test-net/da$i
mkdir -p $d/keys
# v3 authority certificate. -m is a lifetime in months; Tor's tor-gencert
# caps this at 24 (2 years) as of tor 0.4.9.x, so that's what's used here
# instead of a longer "never rot" lifetime — regenerate before it expires.
printf '' | tor-gencert --create-identity-key -m 24 -a 192.168.1.1${i}:9030 \
  -i $d/keys/authority_identity_key \
  -s $d/keys/authority_signing_key \
  -c $d/keys/authority_certificate --passphrase-fd 0
# relay identity (gives the DirAuthority line its trailing fingerprint):
chmod 700 $d
tor --DataDirectory $d --list-fingerprint --orport 9001 \
  --dirauthority "placeholder 127.0.0.1:9030 0000000000000000000000000000000000000000" \
  --quiet
```

`--passphrase-fd 0` reads an empty passphrase from stdin, so the generated
identity key is unencrypted (required since these fixtures are read
unattended by the NixOS test VMs).

Then, for each DA:

- v3 fingerprint: `grep '^fingerprint' $d/keys/authority_certificate`
  (the value after the `fingerprint` keyword).
- relay fingerprint: the second field of `$d/fingerprint` (strip the
  leading nickname and any spaces).

Update `fingerprints.nix` with the six real values, clean up `$d/lock` and
anything else Tor wrote that isn't in the "Layout" list above (notably
`keys/secret_onion_key`, `keys/secret_onion_key_ntor`,
`keys/ed25519_signing_cert`, `keys/ed25519_signing_secret_key` — Tor
regenerates these automatically from the master identity key on next
startup, so they don't need to be fixture material), then commit.

## Cert lifetime note

The `-m 24` above is the *maximum* `tor-gencert` will accept for a v3
authority certificate (tested against tor 0.4.9.11). The certificate
therefore expires 24 months after generation, at which point the private
test network's consensus will stop forming until the certs (and
`fingerprints.nix`, if the identity keys are also rotated) are
regenerated. Since `TestingTorNetwork = true` is set in `torrc.nix`, Tor
relaxes several other normal-network timing constraints, but it does not
relax certificate expiry.
