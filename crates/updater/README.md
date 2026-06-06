# nenjo-updater

Shared update-check and binary bundle installation logic for the Nenjo
command-line tools.

The crate does not define a public command by itself. The shipped updater binary
is `nenjoup`, built by `bin/nenjoup`, while `nenjo update` and `nenpm update`
delegate to that binary.

