# SQLCipher source

SQLCipher 4.19.0, upstream commit
`c4b275a47932888216bade83aff2bbc73df0ff85`:
https://github.com/sqlcipher/sqlcipher/tree/c4b275a47932888216bade83aff2bbc73df0ff85

`sqlite3.c` and `sqlite3.h` are the upstream amalgamation generated with
`nmake /f Makefile.msc sqlite3.c` from that checkout. No application changes
are applied to the generated source. See `LICENSE.md` for SQLCipher's
BSD-3-Clause license and `SQLITE_LICENSE.md` for SQLite's public-domain notice.

SHA-256:

- sqlite3.c: `8640c653acadf665cce6331646f60b5b74a4690746f2c4a2d8f688a0570a0c0c`
- sqlite3.h: `8a9d1bff44d75174ca6dea3ea9bac50a6104d86facb566647b8bb839375b7b3a`

The `sqlcipher-bundled` Cargo feature builds this source and statically links
OpenSSL built by `openssl-src`. Build tools are needed on the build machine;
applications do not need a SQLCipher SDK on the end user's machine.
