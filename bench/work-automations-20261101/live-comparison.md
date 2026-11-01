| Metric | run | run | Delta |
|---|---:|---:|---:|
| success_rate | 1 | 1 | 0 (+0.0%) |
| cost_per_success_usd | 0.021819 | 0.015597 | -0.006222 (-28.5%) |
| cost_usd | 0.021819 | 0.015597 | -0.006222 (-28.5%) |
| model_calls | 3 | 2.750 | -0.250000 (-8.3%) |
| input_tokens | 14162 | 13702.8 | -459.2 (-3.2%) |
| uncached_input_tokens | 3779.8 | 1945.9 | -1833.9 (-48.5%) |
| cached_input_tokens | 10382.2 | 11756.9 | 1374.6 (+13.2%) |
| output_tokens | 1029.8 | 838.5 | -191.2 (-18.6%) |
| total_tokens | 15191.8 | 14541.2 | -650.5 (-4.3%) |
| tool_calls | 1.375 | 1.250 | -0.125000 (-9.1%) |
| tool_failed | 0 | 0 | 0 |
| deduplicated | 0 | 0 | 0 |
| repeated_executions | 0 | 0 | 0 |
| provider_retries | 0 | 0 | 0 |
| steps | 3 | 2.750 | -0.250000 (-8.3%) |
| peak_context_tokens | 1640 | 1640 | 0 (+0.0%) |
| mean_context_tokens | 1379.6 | 1367.8 | -11.875 (-0.9%) |
| tool_schema_tokens | 1845.5 | 1695 | -150.5 (-8.2%) |
| prefix_reuse_tokens | 2738.9 | 2391.8 | -347.1 (-12.7%) |
| median_turn_ms | 7220 | 4734 | -2486 (-34.4%) |
| p95_turn_ms | 14782 | 13277 | -1505 (-10.2%) |
| median_first_token_ms | 1754 | 1730 | -24 (-1.4%) |
| stall_stops | 0 | 0 | 0 |

| Workload | Success | Model calls | Total tokens | Uncached input | Cost/success (USD) |
|---|---:|---:|---:|---:|---:|
| simple | 1 | 1 | 3384 -> 3391.5 | 1335.5 -> 2 | 0.005080 -> 0.002088 |
| multi-step | 1 | 5 | 26282 -> 28302 | 6581 -> 5197.5 | 0.035767 -> 0.033694 |
| duplicate | 1 | 2.500 | 15218.5 -> 15256 | 3084.5 -> 1051.5 | 0.018312 -> 0.014001 |
| approval | 1 | 3.500 -> 2.500 | 15882.5 -> 11215.5 | 4118 -> 1532.5 | 0.028118 -> 0.012605 |
