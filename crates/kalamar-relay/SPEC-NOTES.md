# Specification notes

Cases where the relay protocol's specification (`engine/docs/lan-relay-protocol.md` in the Kalamar repository, v1),
the engine's relay and the engine's client disagree, or where the specification is silent or ambiguous. Each entry
says what this relay does and why. Nothing here changes the bytes on the wire; the test vectors of section 9 are
reproduced byte for byte in `src/wire.rs`.

## Disagreements between the specification and the engine's relay

1. **CLOSED answers to undersized messages.** Section 3 says a message shorter than its size is dropped, and the table
   of section 7.2 allows a CLOSED only in answer to a KEEPALIVE of 28 bytes or a DATA of at least 32 bytes. The
   reference answers CLOSED to any message of 20 bytes or more with an unknown session, which sends 24 bytes to an
   unproven address that sent 20 to 23 (amplification, even if small). *Chosen:* the specification. KEEPALIVE needs 28,
   DATA 32, LEAVE 20 and is never answered; shorter messages are dropped silently.

2. **When a DATA datagram counts as a sign of life.** Section 5.2 marks the member alive only after the source-vip and
   rate-limit checks pass; the reference marks it alive before them. *Chosen:* the specification, so a member whose
   DATA is all dropped (spoofed vip, rate exceeded) does not stay alive by sending it. A DATA that passes the checks
   but has no destination (unknown vip, own vip) still counts: section 5.2 says "otherwise mark the member alive".

3. **Session lookup across rooms for a returning address.** Section 5.1 step 5 ends the old member "when the room has a
   member with this source address". The reference scans only the room named in the new JOIN, so an address that
   joins room TWO while still a member of room ONE would hold two sessions. *Chosen:* one session per source address
   in the whole relay; a JOIN from an address that already has a session (another nonce, any room) ends the old session
   first (MEMBER(left) to its room, no CLOSED). A client has one transport address and one session, and this keeps the
   address-to-session map unambiguous (the rebind rule and "DATA must come from the member's address" need it).

4. **Stored display name.** The reference decodes the name as UTF-8 with replacement and re-encodes it, which can change
   the bytes and their count. *Chosen:* the name is kept and forwarded as the exact bytes received (at most 32, no
   control characters), which is what section 3.3 and 3.11 describe ("informational only").

## Silent or ambiguous in the specification

5. **"Keep no state" for HELLO versus per-source-IP rate limits (section 5.1 vs 7.3).** A per-IP counter is per-source
   state held before any proof. *Chosen:* the HELLO and JOIN counters, and the once-per-second CLOSED limiter, are fixed
   arrays of 4096 slots indexed by a randomly keyed hash of the source (`src/ratelimit.rs`). Memory never depends on the
   number of sources, nothing is created per source, and an attacker cannot aim at a victim's slot (the key is secret).
   Two sources sharing a slot can only cause an extra drop. The tests check that spoofed floods leave no sessions or
   rooms and that the tables keep their size.

6. **JOIN_NAK reason 7 (rate limited) versus "excess dropped" (section 7.3).** *Chosen:* excess HELLO and JOIN are
   dropped silently and reason 7 is never sent: an answer to a flood is itself traffic toward a possibly spoofed
   address. (The reference does the same.) The reason code stays defined in the codec.

7. **Rebind onto an address that belongs to another member.** Section 5.2 lets an authenticated KEEPALIVE from a new
   address move the member, but does not say what happens if another live session already uses that address. *Chosen:*
   the KEEPALIVE is dropped (counted as `rebind_conflict`); otherwise one session could steal another's traffic
   and the address-to-session map would break. A genuine move to an address nobody uses works as specified, and the
   old address stops being accepted for DATA and LEAVE immediately.

8. **Per-room rate limit.** Section 7.3 only has per-IP handshake limits and a per-member DATA limit. *Added:* a
   per-room token bucket on received DATA (default 12 000 datagrams and 16 MiB per second, counted before fan-out),
   configurable and switchable off (`0`). The per-member limits are exactly the specified 2000 datagrams and 4 MiB
   per second, one second deep.

9. **Maximum size of HELLO and JOIN.** The table says JOIN is "at most 104" bytes but section 2 says to ignore bytes
   past the fields needed and to drop only datagrams above 1472. *Chosen:* accept any HELLO or JOIN of 64 to 1472 bytes
   (as the reference does); the answers (32, 24 or 40 bytes) are never larger than the request.

10. **Stability of virtual IPs across a relay restart.** The specification gives the mechanism (the client's JOIN
    requests its previous vip; the relay grants it when it is valid and free) and the reference keeps no state across
    restarts, so this relay persists nothing either. After a restart the client's next KEEPALIVE gets CLOSED reason 2
    (the unknown session) and the client rejoins at once requesting its vip. *Limit:* a brand new client that joins
    the restarted relay before the old members have returned takes the lowest free vip and may take an address that
    an absent member will ask for; that member then receives the next free one. Persistence would remove this and
    would conflict with "no persistence" (README), so it is not done. The interop run shows three peers keeping
    10.13.0.1, .2 and .3 across a kill and restart.

11. **Rate limiting uses the canonical IP.** On a dual-stack socket an IPv4 client appears as `::ffff:a.b.c.d`; cookie
    and rate-limit keys use the IPv4 form so both spellings of one client are the same source. Replies still go to the
    address exactly as received.

12. **Sources that can never be valid.** Datagrams from UDP port 0, an unspecified address or a multicast address are
    dropped before parsing. The specification does not mention it; no client can receive an answer there.

13. **Idle expiry granularity.** "No valid KEEPALIVE, DATA or LEAVE for the idle timeout (30 s)": the member is removed
    when the silence is strictly longer than 30 s, checked once per second, so removal happens between 30 and 32 s after
    the last sign of life (the reference is the same, with a 0.2 s to 1 s check).

14. **Cookie secret rotation.** Section 7.1 gives a 32-byte secret chosen at start. *Added:* it is replaced every hour
    by default (`--cookie-rotation`, minimum 60 s) and the previous secret is kept for one more period, so a cookie
    issued just before a rotation still verifies; no handshake fails because of a rotation.

15. **Windows `WSAECONNRESET` on receive.** After a datagram is sent to a port that has closed, Windows reports ICMP
    "port unreachable" as an error of the next `recv_from` on the same socket. The server ignores that error and
    keeps reading (the reference disables it with `SIO_UDP_CONNRESET`). Without this, a client that vanishes would
    stop the relay.

## Checked against the client, no difference found

- The engine's client requests its current vip on every rejoin, treats CLOSED with its own session id and token as
  "join again at once", and ignores relay messages that do not carry its own session id and token; the relay always
  puts the recipient's session header on DATA, MEMBER, KEEPALIVE_ACK and CLOSED.
- The client sends KEEPALIVE every 5 s and declares the relay dead after 15 s; the relay's 30 s idle timeout is longer.
