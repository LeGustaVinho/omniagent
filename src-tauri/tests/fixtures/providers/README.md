# Provider stream fixtures

The thinking-only Claude block and the checks for a completion and usage on
each successive turn adapt the cases `turn_result_usage_is_emitted_per_turn`
and the thinking and unknown-frame tests in
[zag-agent/src/streaming_tests.rs](https://github.com/niclaslindstedt/zag/blob/54c70b93c7942b247acb98f8988e198aa44e2904/zag-agent/src/streaming_tests.rs).
Capability checks retain the distinction between provider evidence, adapter
behavior, and unavailable functionality from
[zag-agent/src/capability.rs](https://github.com/niclaslindstedt/zag/blob/54c70b93c7942b247acb98f8988e198aa44e2904/zag-agent/src/capability.rs).
The upstream license is included in `LICENSE-zag`.

App Server and ACP v1 use separate frames from their own contracts. The ACP
fixture represents delegation as a tool call because this stream supplies no
native child identity. The prompt response uses the request ID sent by the
adapter. These synthetic streams provide fixture evidence only.
