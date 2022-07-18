window.BENCHMARK_DATA = {
  "lastUpdate": 1658139894508,
  "repoUrl": "https://github.com/rmw-lib/simpledb",
  "entries": {
    "Benchmark": [
      {
        "commit": {
          "author": {
            "email": "i@rmw.link",
            "name": "gcxfd",
            "username": "gcxfd"
          },
          "committer": {
            "email": "i@rmw.link",
            "name": "gcxfd",
            "username": "gcxfd"
          },
          "distinct": true,
          "id": "e3c9b72fd9dd1d272765982aff6451bd6f2067b5",
          "message": "cargo +nightly clippy && use criterion for benchmark",
          "timestamp": "2022-07-18T18:00:50+08:00",
          "tree_id": "33a9525885ca48daff07fc8af96dcb4ad15f769b",
          "url": "https://github.com/rmw-lib/simpledb/commit/e3c9b72fd9dd1d272765982aff6451bd6f2067b5"
        },
        "date": 1658139893998,
        "tool": "cargo",
        "benches": [
          {
            "name": "map_put",
            "value": 18764,
            "range": "± 1717",
            "unit": "ns/iter"
          },
          {
            "name": "map_get",
            "value": 17122,
            "range": "± 182",
            "unit": "ns/iter"
          }
        ]
      }
    ]
  }
}