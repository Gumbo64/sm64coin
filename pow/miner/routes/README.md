# Reference routes

Unperturbed runs from power-on to a star, used by the automated miner as
paths to follow through the perturbation (`--route`, tried in order). All
come from the objective-rrt searches in `sm64-AI-binds` (none are TAS
inputs); most take BoB's "Behind Chain Chomp's Gate" star.

| file | source | star at frame |
|---|---|---|
| block1-human.m64 | the SM64Coin chain's block 1: a human run with the web miner (pound the chomp's post, then jump to the star). The default: easiest to follow | 4329 |
| calm-first-star.m64 | objective-rrt runs/pk5-ps-s2 first-Mission-ReachStar | 3577 (bomb boost over the gate) |
| chomp-a.m64 | objective-rrt-v2 logs/mend/q15-s2-star4-7937 | 2988 |
| chomp-b.m64 | objective-rrt runs/legs-indef14 leg1-s1-best | 3063 |
| chomp-c.m64 | objective-rrt-v2 runs/q6-rrfobnb-s1 first-Chest-Open | 3199 |
| chomp-d.m64 | objective-rrt-v2 runs/q8-pfo-s1 polish-best | 3205 |
| chomp-e.m64 | objective-rrt runs/route-dev-perturb-s1 best | 4793 |
| king-bobomb.m64 | objective-rrt-v2 kingloop best-fast-king-tree-d | 3829 (hyperspeed + cannon: hard to follow) |

Any m64 that meets the rules' success condition unperturbed works; the
miner checks each at startup.
