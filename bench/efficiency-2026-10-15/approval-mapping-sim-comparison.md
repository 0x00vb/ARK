| Metric | baseline-sim | approval-mapping-sim | Delta |
|---|---:|---:|---:|
| success_rate | 1 | 1 | 0 (+0.0%) |
| cost_per_success_usd | 0.081710 | 0.082701 | 0.000991 (+1.2%) |
| cost_usd | 0.081710 | 0.082701 | 0.000991 (+1.2%) |
| model_calls | 72.455 | 72.455 | 0 (+0.0%) |
| input_tokens | 118003.6 | 122135.1 | 4131.5 (+3.5%) |
| uncached_input_tokens | 19926.7 | 19959.4 | 32.727 (+0.2%) |
| cached_input_tokens | 98077.0 | 102175.7 | 4098.8 (+4.2%) |
| output_tokens | 1553.3 | 1553.3 | 0 (+0.0%) |
| total_tokens | 119557.0 | 123688.5 | 4131.5 (+3.5%) |
| tool_calls | 24.591 | 24.591 | 0 (+0.0%) |
| tool_failed | 0.090909 | 0.090909 | 0 (+0.0%) |
| deduplicated | 0.090909 | 0.090909 | 0 (+0.0%) |
| repeated_executions | 0.090909 | 0.090909 | 0 (+0.0%) |
| provider_retries | 0 | 0 | 0 |
| steps | 72.364 | 72.364 | 0 (+0.0%) |
| peak_context_tokens | 6643 | 6701 | 58 (+0.9%) |
| mean_context_tokens | 1714.7 | 1791.0 | 76.273 (+4.4%) |
| tool_schema_tokens | 40921.1 | 40875.7 | -45.455 (-0.1%) |
| prefix_reuse_tokens | 98239.5 | 102340.9 | 4101.4 (+4.2%) |
| median_turn_ms | 1 | 2 | 1 (+100.0%) |
| p95_turn_ms | 2 | 3 | 1 (+50.0%) |
| median_first_token_ms | 0 | 0 | 0 |
| stall_stops | 0.045455 | 0.045455 | 0 (+0.0%) |

| Workload | Success | Model calls | Total tokens | Uncached input | Cost/success (USD) |
|---|---:|---:|---:|---:|---:|
| simple | 1 | 1 | 1165 -> 1222 | 1162 -> 1219 | 0.002927 -> 0.003070 |
| tool-lookup | 1 | 3 | 4115 -> 4286 | 1492 -> 1549 | 0.004854 -> 0.005019 |
| multi-step | 1 | 5 | 7413 -> 7698 | 1853 -> 1906 | 0.006836 -> 0.007014 |
| large-output | 1 | 4 | 5435 -> 5663 | 1562 -> 1617 | 0.005562 -> 0.005735 |
| long-session | 1 | 1500 | 2461151 -> 2546651 | 381684 -> 380709 | 1.605 -> 1.620 |
| failure-recovery | 1 | 4 | 5785 -> 6013 | 1705 -> 1760 | 0.005716 -> 0.005888 |
| duplicate | 1 | 4 | 5590 -> 5818 | 1592 -> 1647 | 0.005443 -> 0.005615 |
| runaway | 1 | 7 | 11075 -> 11474 | 3603 -> 3712 | 0.011560 -> 0.011891 |
| resume | 1 | 6 | 8428 -> 8770 | 1896 -> 1949 | 0.007251 -> 0.007441 |
| retrieval | 1 | 3 | 3791 -> 3965 | 2372 -> 2488 | 0.006199 -> 0.006471 |
| approval | 1 | 3 | 3974 -> 4145 | 1435 -> 1490 | 0.004660 -> 0.004821 |
| parallel | 1 | 2 | 2697 -> 2811 | 1425 -> 1481 | 0.004458 -> 0.004610 |
| stale-read | 1 | 4 | 5514 -> 5742 | 1591 -> 1646 | 0.005343 -> 0.005515 |
| team | 1 | 11 | 15358 -> 15991 | 6529 -> 6757 | 0.021630 -> 0.022281 |
| skill | 1 | 3 | 4540 -> 4714 | 1663 -> 1722 | 0.005493 -> 0.005664 |
| web | 1 | 5 | 9458 -> 9748 | 2404 -> 2463 | 0.008636 -> 0.008830 |
| claimed-files | 1 | 5 | 8148 -> 8433 | 2120 -> 2174 | 0.008211 -> 0.008392 |
| claimed-schedule | 1 | 6 | 9256 -> 9604 | 3371 -> 3487 | 0.010911 -> 0.011247 |
| board | 1 | 6 | 16075 -> 16417 | 6855 -> 7082 | 0.020164 -> 0.022755 |
| analysis | 1 | 3 | 5107 -> 5281 | 1926 -> 1984 | 0.007508 -> 0.007676 |
| files-edit | 1 | 5 | 13675 -> 13965 | 3194 -> 3253 | 0.011710 -> 0.011904 |
| workbooks | 1 | 4 | 22503 -> 22735 | 6953 -> 7012 | 0.027864 -> 0.028046 |

Regressions:
- simple: cost_per_success_usd 0.002927 -> 0.003070
- simple: input_tokens 1162 -> 1219
- simple: uncached_input_tokens 1162 -> 1219
- simple: total_tokens 1165 -> 1222
- simple: peak_context_tokens 1162 -> 1219
- tool-lookup: cost_per_success_usd 0.004854 -> 0.005019
- tool-lookup: input_tokens 4049 -> 4220
- tool-lookup: uncached_input_tokens 1492 -> 1549
- tool-lookup: total_tokens 4115 -> 4286
- tool-lookup: peak_context_tokens 1413 -> 1470
- multi-step: cost_per_success_usd 0.006836 -> 0.007014
- multi-step: input_tokens 7291 -> 7576
- multi-step: uncached_input_tokens 1853 -> 1906
- multi-step: total_tokens 7413 -> 7698
- multi-step: peak_context_tokens 1609 -> 1666
- large-output: cost_per_success_usd 0.005562 -> 0.005735
- large-output: input_tokens 5339 -> 5567
- large-output: uncached_input_tokens 1562 -> 1617
- large-output: total_tokens 5435 -> 5663
- large-output: peak_context_tokens 1439 -> 1496
- long-session: cost_per_success_usd 1.605 -> 1.620
- long-session: input_tokens 2430173 -> 2515673
- long-session: total_tokens 2461151 -> 2546651
- long-session: peak_context_tokens 1676 -> 1733
- failure-recovery: cost_per_success_usd 0.005716 -> 0.005888
- failure-recovery: input_tokens 5712 -> 5940
- failure-recovery: uncached_input_tokens 1705 -> 1760
- failure-recovery: total_tokens 5785 -> 6013
- failure-recovery: peak_context_tokens 1533 -> 1590
- duplicate: cost_per_success_usd 0.005443 -> 0.005615
- duplicate: input_tokens 5517 -> 5745
- duplicate: uncached_input_tokens 1592 -> 1647
- duplicate: total_tokens 5590 -> 5818
- duplicate: peak_context_tokens 1460 -> 1517
- runaway: cost_per_success_usd 0.011560 -> 0.011891
- runaway: input_tokens 10955 -> 11354
- runaway: uncached_input_tokens 3603 -> 3712
- runaway: total_tokens 11075 -> 11474
- runaway: peak_context_tokens 1900 -> 1957
- resume: cost_per_success_usd 0.007251 -> 0.007441
- resume: input_tokens 8294 -> 8636
- resume: uncached_input_tokens 1896 -> 1949
- resume: total_tokens 8428 -> 8770
- resume: peak_context_tokens 1469 -> 1526
- retrieval: cost_per_success_usd 0.006199 -> 0.006471
- retrieval: input_tokens 3747 -> 3921
- retrieval: uncached_input_tokens 2372 -> 2488
- retrieval: total_tokens 3791 -> 3965
- retrieval: peak_context_tokens 1505 -> 1563
- approval: cost_per_success_usd 0.004660 -> 0.004821
- approval: input_tokens 3912 -> 4083
- approval: uncached_input_tokens 1435 -> 1490
- approval: total_tokens 3974 -> 4145
- approval: peak_context_tokens 1363 -> 1420
- parallel: cost_per_success_usd 0.004458 -> 0.004610
- parallel: input_tokens 2629 -> 2743
- parallel: uncached_input_tokens 1425 -> 1481
- parallel: total_tokens 2697 -> 2811
- parallel: peak_context_tokens 1393 -> 1450
- stale-read: cost_per_success_usd 0.005343 -> 0.005515
- stale-read: input_tokens 5448 -> 5676
- stale-read: uncached_input_tokens 1591 -> 1646
- stale-read: total_tokens 5514 -> 5742
- stale-read: peak_context_tokens 1444 -> 1501
- team: cost_per_success_usd 0.021630 -> 0.022281
- team: input_tokens 14955 -> 15588
- team: uncached_input_tokens 6529 -> 6757
- team: total_tokens 15358 -> 15991
- team: peak_context_tokens 1589 -> 1647
- skill: cost_per_success_usd 0.005493 -> 0.005664
- skill: input_tokens 4456 -> 4630
- skill: uncached_input_tokens 1663 -> 1722
- skill: total_tokens 4540 -> 4714
- skill: peak_context_tokens 1575 -> 1633
- web: cost_per_success_usd 0.008636 -> 0.008830
- web: input_tokens 9325 -> 9615
- web: uncached_input_tokens 2404 -> 2463
- web: total_tokens 9458 -> 9748
- web: peak_context_tokens 2189 -> 2247
- claimed-files: cost_per_success_usd 0.008211 -> 0.008392
- claimed-files: input_tokens 7958 -> 8243
- claimed-files: uncached_input_tokens 2120 -> 2174
- claimed-files: total_tokens 8148 -> 8433
- claimed-files: peak_context_tokens 1797 -> 1854
- claimed-schedule: cost_per_success_usd 0.010911 -> 0.011247
- claimed-schedule: input_tokens 9110 -> 9458
- claimed-schedule: uncached_input_tokens 3371 -> 3487
- claimed-schedule: total_tokens 9256 -> 9604
- claimed-schedule: peak_context_tokens 1618 -> 1676
- board: cost_per_success_usd 0.020164 -> 0.022755
- board: input_tokens 15939 -> 16281
- board: uncached_input_tokens 6855 -> 7082
- board: total_tokens 16075 -> 16417
- board: peak_context_tokens 2796 -> 2853
- analysis: cost_per_success_usd 0.007508 -> 0.007676
- analysis: input_tokens 4884 -> 5058
- analysis: uncached_input_tokens 1926 -> 1984
- analysis: total_tokens 5107 -> 5281
- analysis: peak_context_tokens 1754 -> 1812
- files-edit: cost_per_success_usd 0.011710 -> 0.011904
- files-edit: input_tokens 13495 -> 13785
- files-edit: uncached_input_tokens 3194 -> 3253
- files-edit: total_tokens 13675 -> 13965
- files-edit: peak_context_tokens 2931 -> 2989
- workbooks: cost_per_success_usd 0.027864 -> 0.028046
- workbooks: input_tokens 21730 -> 21962
- workbooks: uncached_input_tokens 6953 -> 7012
- workbooks: total_tokens 22503 -> 22735
- workbooks: peak_context_tokens 6643 -> 6701
