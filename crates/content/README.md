# nenjo-content

Storage-neutral value types shared by Nenjo's core runtime, tool, model, event,
and session contracts.

This crate deliberately contains no platform client, authorization, cache,
filesystem, decryption, or model-provider behavior. Runtime implementations
consume these validated values at their respective integration boundaries.
