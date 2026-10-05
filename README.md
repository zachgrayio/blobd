# blobd

Extremely fast object storage, performing at raw device speeds. Achieves constant sub-millisecond latency random partial reads over objects regardless of size.

<img src="./README.write-throughput.png">
<img src="./README.random-read-ops.png">

Read the [blog post](https://blog.wilsonl.in/blobd) for an accessible deep dive.

## Usage

blobd requires Linux 6.11+, a NVMe SSD with support for atomic writes of 512-byte blocks, and direct access to the raw block device. Install the server by using `cargo`:

```bash
cargo install blobd
```

Then set up the configuration file:

```yaml
device:
  path: /dev/my-nvme-device
  offset: 0
  len: <length of block device in bytes>
token_secret_base64: abcdef1234567890abcdef1234567890
interface: 127.0.0.1
port: 8080
```

Now you can format the device and start the server:

```bash
blobd --config my-config.yaml --format
blobd --config my-config.yaml
```

The server can now be accessed over HTTP. Clients are available for [Node.js](./blobd-client-js/) and [Rust](./blobd-client-rs/).

### Bounded cache mode

In the server's TOML configuration, set `cache_eviction = true` to evict committed objects with CLOCK when the configured `device.len` partition fills. Incomplete uploads and active read streams are not evictable. Evicted keys return HTTP 404; a write that cannot make space returns 507. A successful data read touches CLOCK; inspect does not. The library's `touch_object` method provides an explicit, metadata-only touch. This mode is opt-in because eviction permanently removes committed data.

For performance comparisons, run the `benchmark-runner` clock workload with CLOCK off and on (`--cache-eviction`); `--cache-pressure` tests eviction. The runner formats the configured device.

To run the CLOCK comparison on macOS with Docker, use `bash benchmark-runner/docker-compare.sh` from the repository root. Docker must support `--privileged` and loop devices. The script creates a disposable 32 MiB image inside the container, runs the direct-device tests, checks the same seeded stochastic workload with CLOCK off and on, then runs below-capacity benchmarks and a 2 MiB eviction-pressure benchmark. It writes benchmark JSON and stresser logs to `benchmark-runner/cfg/clock/`; no host block device is formatted. These are VM loop-device measurements, not physical-NVMe results.

Use `bash benchmark-runner/docker-compare.sh --suite` for three repetitions of six below-capacity profiles (4, 16, and 64 KiB objects at serial and eight-way concurrency), alternating CLOCK-off and CLOCK-on order, plus 2 MiB and 4 MiB eviction-pressure profiles. Each run formats the disposable loop partition. The timestamped `benchmark-runner/cfg/clock/runs/` directory keeps every JSON result, log, `samples.jsonl`, `medians.tsv`, and a `plots/index.html` report with SVG images for every recorded numeric metric. Eviction pressure can only run with CLOCK on; the old store interface panics on expected cache misses. Results measure a warm-cache Docker VM, not bare-metal storage. Stochastic runs use the same seed but concurrent scheduling still varies.

## History

This project used to be called *Turbostore* and was written entirely in C; you can still see the code [here](https://github.com/wilsonzlin/blobd/tree/ffb637ae4e4e91602ec04cf2fb2b50aafa116876).
