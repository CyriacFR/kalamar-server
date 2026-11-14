# Upgrading a server

How to move a running server to a newer version of this repository, and how to go back. Each release carries notes on
the repository's Releases page: read them first, they say whether the version adds a database migration or changes a
default.

## 1. Back up

Stop nothing yet. Copy, to a place outside the server:

- the database (`server.database`; with SQLite's write-ahead log, copy the `-wal` and `-shm` files with it, or stop
  the service for the copy);
- `server.secret_file` (without it, logins cannot find their accounts);
- every relay secret (`relay.key` files) and the configuration file;
- the two binaries you run, so that going back is a copy.

## 2. Build and install the new programs

```sh
cargo build --release --locked -p kalamar-service -p kalamar-relay
```

The service you run keeps running while this builds. With Docker, build the new image under a new tag.

## 3. Check the configuration

```sh
./target/release/kalamar-service --config /etc/kalamar/kalamar.toml check-config
```

The new binary reads your file and changes nothing. It prints `OK`, with warnings you may accept, or
`configuration error` lines to fix before going on. The configuration refuses unknown keys: a key is never renamed or
removed between versions, and new keys are optional.

## 4. Replace and restart

Stop the service, install the new binaries (and the units of `deploy/systemd/` if the release notes say they
changed), then start the relay and the service. Migrations run when the service starts; `kalamar-service migrate`
runs them alone, the service stopped. Run the service and its admin commands as the account that owns the database,
never as another one: files created by another account stop the service from writing.

The admin commands never migrate: run against a database an older service still serves, they stop and say so.

## 5. Verify

- [ ] `check-config` prints only the warnings you chose to keep
- [ ] the relay's log says `bound=true`, or you meant `--open`
- [ ] `kalamar-service --config ... admin relays` shows each relay answering
- [ ] a match between two players of your server reaches its result
- [ ] on a test instance, `kalamar-testkit conformance --api https://test.example.org/api` says `CONFORMANT`

## 6. Going back

A version that added no migration: stop, put the previous binaries back, start. A version that added one: a
database migrated by a newer version is refused by an older program, with the schema number it found and its own.
Stop the service and restore the backup of step 1 (what happened since is lost), or keep the newer version.

## Defaults to know in 1.1.13

The first public revision:

- **The relay needs a secret, or `--open`.** `kalamar-relay` refuses to start without `--room-secret-file` (a relay
  bound to your service: `kalamar-relay --new-room-secret /etc/kalamar/relay.key` makes the file, and the service's
  `[[relays]]` entry names the same secret in `secret_file`) or `--open` (a relay anyone may use, for a LAN party).
  With the units of `deploy/systemd/`, systemd hands the root-only file to each program (`LoadCredential`): the relay
  unit has the line, the service takes it from a drop-in
  ([operator-first-steps.md](operator-first-steps.md#4-run-both-programs)).
- **The relay uses every processor.** One worker per processor by default (`--workers`), 1024 sessions per worker and
  a quarter as many rooms (`--max-sessions`, `--max-rooms`; `0` means no cap), a 256 KiB receive buffer
  (`--recv-buffer-kb`). `[embedded_relay] workers` does the same for the relay inside the service.
- **The service asks each bound relay for its load** every 5 seconds (`[relay_load]`), over UDP on the relay's port:
  a full or silent relay gets no new match while another has room, and when every relay is full a room start gets
  `503 relay_capacity` ([relay.md](protocol/relay.md#43-relays-that-announce-their-load-1113)). The service must
  reach each relay's UDP port from its own address.
- **How many players a machine carries**: [operator-first-steps.md](operator-first-steps.md#capacity-of-a-relay).
