| Metric | before-f87618c | after-work-automations | Delta |
|---|---:|---:|---:|
| success_rate | 1 | 1 | 0 (+0.0%) |
| cost_per_success_usd | 0.082755 | 0.082755 | 0 (+0.0%) |
| cost_usd | 0.082755 | 0.082755 | 0 (+0.0%) |
| model_calls | 72.455 | 72.455 | 0 (+0.0%) |
| input_tokens | 122173.9 | 122173.9 | 0 (+0.0%) |
| uncached_input_tokens | 19977.6 | 19977.6 | 0 (+0.0%) |
| cached_input_tokens | 102196.2 | 102196.2 | 0 (+0.0%) |
| output_tokens | 1553.6 | 1553.6 | 0 (+0.0%) |
| total_tokens | 123727.5 | 123727.5 | 0 (+0.0%) |
| tool_calls | 24.591 | 24.591 | 0 (+0.0%) |
| tool_failed | 0.090909 | 0.090909 | 0 (+0.0%) |
| deduplicated | 0.090909 | 0.090909 | 0 (+0.0%) |
| repeated_executions | 0.090909 | 0.090909 | 0 (+0.0%) |
| provider_retries | 0 | 0 | 0 |
| steps | 72.364 | 72.364 | 0 (+0.0%) |
| peak_context_tokens | 6701 | 6701 | 0 (+0.0%) |
| mean_context_tokens | 1797.4 | 1797.4 | 0 (+0.0%) |
| tool_schema_tokens | 40909.4 | 40909.4 | 0 (+0.0%) |
| prefix_reuse_tokens | 102361.9 | 102361.9 | 0 (+0.0%) |
| median_turn_ms | 2 | 2 | 0 (+0.0%) |
| p95_turn_ms | 3 | 3 | 0 (+0.0%) |
| median_first_token_ms | 0 | 0 | 0 |
| stall_stops | 0.045455 | 0.045455 | 0 (+0.0%) |

| Workload | Success | Model calls | Total tokens | Uncached input | Cost/success (USD) |
|---|---:|---:|---:|---:|---:|
| simple | 1 | 1 | 1222 | 1219 | 0.003070 |
| tool-lookup | 1 | 3 | 4286 | 1549 | 0.005019 |
| multi-step | 1 | 5 | 7698 | 1906 | 0.007014 |
| large-output | 1 | 4 | 5663 | 1617 | 0.005735 |
| long-session | 1 | 1500 | 2546651 | 380709 | 1.620 |
| failure-recovery | 1 | 4 | 6013 | 1760 | 0.005888 |
| duplicate | 1 | 4 | 5818 | 1647 | 0.005615 |
| runaway | 1 | 7 | 11474 | 3712 | 0.011891 |
| resume | 1 | 6 | 8770 | 1949 | 0.007441 |
| retrieval | 1 | 3 | 3965 | 2488 | 0.006471 |
| approval | 1 | 3 | 4145 | 1490 | 0.004821 |
| parallel | 1 | 2 | 2811 | 1481 | 0.004610 |
| stale-read | 1 | 4 | 5742 | 1646 | 0.005515 |
| team | 1 | 11 | 15984 | 6757 | 0.022279 |
| skill | 1 | 3 | 4714 | 1722 | 0.005664 |
| web | 1 | 5 | 9748 | 2463 | 0.008830 |
| claimed-files | 1 | 5 | 8433 | 2174 | 0.008392 |
| claimed-schedule | 1 | 6 | 9604 | 3487 | 0.011247 |
| board | 1 | 6 | 17282 | 7483 | 0.023936 |
| analysis | 1 | 3 | 5281 | 1984 | 0.007676 |
| files-edit | 1 | 5 | 13965 | 3253 | 0.011904 |
| workbooks | 1 | 4 | 22735 | 7012 | 0.028046 |
