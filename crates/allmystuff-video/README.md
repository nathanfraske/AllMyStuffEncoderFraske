# allmystuff-video

Video processing shared by AllMyStuff callers. The default feature set provides
metadata, timing, pacing, access-unit classification, closing markers and
route-local receive policy without a native codec, capture device or transport.

`framing::split_annexb_paced_host` and `framing::split_annexb_paced_stub` preserve
the two existing walks, including their different malformed-prefix behavior.
`codec::sniff_codec` classifies the original byte patterns; it is not a validator
or a declaration that a decoder backend exists. AV1 decoding remains a stub.

The caller owns peer authentication, route binding, IPC headers, queue admission,
recovery requests and process supervision. `receive::accept_paced_fragment`
retains the route fallback's existing chunk/byte limits and lack of age expiry;
the daemon ingress adapter has a separate age policy.

This staged extraction is not yet a completed worker/backend migration.
Central compile and compatibility validation is pending.
