The XDP Rust access library.

## Motivation

For the Linux AF_XDP all existing libraries are based on or around the C access
libraries. The goal is develop a Rust centric library that can take advantage
of its added thread-safety benefits for socket types, as well as high-level
abstractions (such as closures, `Arc`) for interacting with the packet buffers.

The primary metrics for decision making are performance, and latency.

## Overview

Goals:
- No more latency than the C implementation in the data paths.
- Enable and simplify *correct* multi-threading on the same Umem.

Non-Goals:
- Handling BPF / XSK_MAP. This is _necessary_ to accept packets on any of the
  RX sockets created, however it can be setup at any point with no interaction
  with the actual queues. Hence we keep this large dependency tree separate.
  (You could choose a pure-Rust libbpf alternative if you want to).
- Dealing with any aspects of buffer allocation.

## Name Origin

The drug Ixabepilone is a pharmaceutical against cancer.
