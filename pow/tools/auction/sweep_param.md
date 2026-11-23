| rules | halving s | scenario | mean s | p50 | p90 | p99 | CV | restarts | stalls | shares |
|---|---|---|---|---|---|---|---|---|---|---|
| current | grace 600 | honest, FTL 15 s | 292.3 | 55.8 | 866.6 | 2483.3 | 1.726 | 12.3 | 0.0 | optimizer bot 42%, quick bot 57%, restart 1% |
| current | grace 600 | hold policy | 290.0 | 58.7 | 824.0 | 3029.1 | 1.7 | 12.3 | 0.0 | optimizer bot 43%, quick bot 56%, restart 1% |
| current | grace 600 | quick bot stamps +120 s | 292.3 | 55.8 | 866.6 | 2483.3 | 1.726 | 12.3 | 0.0 | optimizer bot 42%, quick bot 57%, restart 1% |
| current | grace 600 | quick bot stamps +120 s, hold | 290.0 | 58.7 | 824.0 | 3029.1 | 1.7 | 12.3 | 0.0 | optimizer bot 43%, quick bot 56%, restart 1% |
| current | grace 600 | withhold 120 s | 296.8 | 110.7 | 835.6 | 2559.7 | 1.624 | 12.3 | 0.0 | optimizer bot 55%, quick bot 44%, restart 1% |
| current | grace 600 | withhold 120 s + stale 20 s | 294.0 | 47.0 | 861.9 | 3025.2 | 1.75 | 13.0 | 0.0 | optimizer bot 36%, quick bot 63%, restart 1% |
| auction | 15 | honest, FTL 15 s | 268.4 | 270.5 | 336.1 | 385.3 | 0.202 | 28.3 | 0.0 | optimizer bot 88%, quick bot 10%, restart 2% |
| auction | 15 | hold policy | 269.0 | 269.7 | 335.7 | 387.8 | 0.196 | 33.0 | 0.0 | optimizer bot 88%, quick bot 9%, restart 2% |
| auction | 15 | quick bot stamps +120 s | 268.8 | 267.4 | 330.9 | 429.5 | 0.201 | 12.7 | 0.0 | optimizer bot 0%, quick bot 98%, restart 1% |
| auction | 15 | quick bot stamps +120 s, hold | 269.0 | 269.7 | 335.7 | 387.8 | 0.196 | 33.0 | 0.0 | optimizer bot 88%, quick bot 9%, restart 2% |
| auction | 15 | withhold 120 s | 268.7 | 270.3 | 334.8 | 380.8 | 0.196 | 30.0 | 0.0 | optimizer bot 55%, quick bot 42%, restart 2% |
| auction | 15 | withhold 120 s + stale 20 s | 268.8 | 269.7 | 333.2 | 379.3 | 0.19 | 31.0 | 0.0 | optimizer bot 56%, quick bot 42%, restart 2% |
| auction | 30 | honest, FTL 15 s | 290.5 | 287.3 | 410.5 | 496.4 | 0.312 | 29.3 | 0.0 | optimizer bot 93%, quick bot 5%, restart 2% |
| auction | 30 | hold policy | 290.1 | 287.6 | 407.9 | 506.2 | 0.316 | 26.3 | 0.0 | optimizer bot 93%, quick bot 6%, restart 2% |
| auction | 30 | quick bot stamps +120 s | 292.8 | 287.5 | 403.1 | 555.9 | 0.309 | 12.7 | 0.0 | optimizer bot 0%, quick bot 99%, restart 1% |
| auction | 30 | quick bot stamps +120 s, hold | 290.1 | 287.6 | 407.9 | 506.2 | 0.316 | 26.3 | 0.0 | optimizer bot 93%, quick bot 6%, restart 2% |
| auction | 30 | withhold 120 s | 290.8 | 290.2 | 408.6 | 496.0 | 0.312 | 37.0 | 0.0 | optimizer bot 57%, quick bot 40%, restart 3% |
| auction | 30 | withhold 120 s + stale 20 s | 291.5 | 290.3 | 405.3 | 488.2 | 0.302 | 34.7 | 0.0 | optimizer bot 57%, quick bot 41%, restart 3% |
| auction | 60 | honest, FTL 15 s | 297.9 | 282.1 | 508.4 | 703.1 | 0.526 | 26.0 | 0.0 | optimizer bot 90%, quick bot 8%, restart 2% |
| auction | 60 | hold policy | 296.2 | 283.8 | 494.0 | 689.3 | 0.519 | 20.3 | 0.0 | optimizer bot 90%, quick bot 9%, restart 1% |
| auction | 60 | quick bot stamps +120 s | 300.9 | 287.6 | 509.2 | 736.6 | 0.534 | 14.3 | 0.0 | optimizer bot 2%, quick bot 97%, restart 1% |
| auction | 60 | quick bot stamps +120 s, hold | 296.2 | 283.8 | 494.0 | 689.3 | 0.519 | 20.3 | 0.0 | optimizer bot 90%, quick bot 9%, restart 1% |
| auction | 60 | withhold 120 s | 298.4 | 284.2 | 504.9 | 698.2 | 0.518 | 30.0 | 0.0 | optimizer bot 56%, quick bot 42%, restart 2% |
| auction | 60 | withhold 120 s + stale 20 s | 299.0 | 292.7 | 509.2 | 682.4 | 0.525 | 27.3 | 0.0 | optimizer bot 56%, quick bot 42%, restart 2% |
| auction | 120 | honest, FTL 15 s | 303.4 | 248.8 | 648.3 | 1047.9 | 0.816 | 25.0 | 0.0 | optimizer bot 76%, quick bot 22%, restart 2% |
| auction | 120 | hold policy | 302.0 | 255.8 | 628.3 | 1037.2 | 0.793 | 17.3 | 0.0 | optimizer bot 78%, quick bot 21%, restart 1% |
| auction | 120 | quick bot stamps +120 s | 303.6 | 250.1 | 658.0 | 1076.4 | 0.839 | 17.0 | 0.0 | optimizer bot 28%, quick bot 71%, restart 1% |
| auction | 120 | quick bot stamps +120 s, hold | 302.0 | 255.8 | 628.3 | 1037.2 | 0.793 | 17.3 | 0.0 | optimizer bot 78%, quick bot 21%, restart 1% |
| auction | 120 | withhold 120 s | 301.4 | 260.3 | 621.8 | 1033.0 | 0.798 | 27.0 | 0.0 | optimizer bot 57%, quick bot 41%, restart 2% |
| auction | 120 | withhold 120 s + stale 20 s | 303.4 | 250.7 | 655.4 | 976.0 | 0.811 | 21.0 | 0.0 | optimizer bot 50%, quick bot 48%, restart 2% |
| auction | 300 | honest, FTL 15 s | 308.8 | 170.5 | 799.3 | 1847.8 | 1.257 | 15.7 | 0.0 | optimizer bot 58%, quick bot 40%, restart 1% |
| auction | 300 | hold policy | 302.4 | 165.1 | 758.1 | 1867.6 | 1.276 | 17.0 | 0.0 | optimizer bot 58%, quick bot 41%, restart 1% |
| auction | 300 | quick bot stamps +120 s | 310.0 | 165.3 | 818.1 | 1834.1 | 1.259 | 14.0 | 0.0 | optimizer bot 47%, quick bot 52%, restart 1% |
| auction | 300 | quick bot stamps +120 s, hold | 302.4 | 165.1 | 758.1 | 1867.6 | 1.276 | 17.0 | 0.0 | optimizer bot 58%, quick bot 41%, restart 1% |
| auction | 300 | withhold 120 s | 302.6 | 162.3 | 750.6 | 1737.8 | 1.213 | 16.3 | 0.0 | optimizer bot 60%, quick bot 38%, restart 1% |
| auction | 300 | withhold 120 s + stale 20 s | 305.4 | 141.7 | 790.0 | 1687.2 | 1.237 | 18.0 | 0.0 | optimizer bot 45%, quick bot 54%, restart 1% |
| auction | 1200 | honest, FTL 15 s | 323.0 | 49.0 | 845.1 | 4045.7 | 2.386 | 9.0 | 3.3 | optimizer bot 39%, quick bot 60%, restart 1%, stall 0% |
| auction | 1200 | hold policy | 321.0 | 45.3 | 784.5 | 4357.1 | 2.429 | 9.0 | 4.7 | optimizer bot 38%, quick bot 61%, restart 1%, stall 0% |
| auction | 1200 | quick bot stamps +120 s | 326.9 | 39.2 | 867.2 | 4464.1 | 2.374 | 7.3 | 4.3 | optimizer bot 36%, quick bot 63%, restart 1%, stall 0% |
| auction | 1200 | quick bot stamps +120 s, hold | 321.0 | 45.3 | 784.5 | 4357.1 | 2.429 | 9.0 | 4.7 | optimizer bot 38%, quick bot 61%, restart 1%, stall 0% |
| auction | 1200 | withhold 120 s | 324.6 | 104.6 | 767.6 | 3568.7 | 2.192 | 10.0 | 1.7 | optimizer bot 49%, quick bot 50%, restart 1%, stall 0% |
| auction | 1200 | withhold 120 s + stale 20 s | 333.0 | 39.2 | 829.1 | 4430.7 | 2.372 | 9.3 | 3.3 | optimizer bot 32%, quick bot 67%, restart 1%, stall 0% |
