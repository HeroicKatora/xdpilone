The XDP Rust access library.

## Motivation

For the Linux AF_XDP all existing libraries are based on or around the C access
libraries. The goal is develop a Rust centric library that can take advantage
of its added thread-safety benefits for socket types, as well as high-level
abstractions (such as closures, `Arc`) for interacting with the packet buffers.

The primary metrics for decision making are performance, and latency.

## Overview


