window.BENCHMARK_DATA = {
  "lastUpdate": 1774964467411,
  "repoUrl": "https://github.com/ZcashFoundation/zebra",
  "entries": {
    "Benchmark": [
      {
        "commit": {
          "author": {
            "email": "oxarbitrage@gmail.com",
            "name": "Alfredo Garcia",
            "username": "oxarbitrage"
          },
          "committer": {
            "email": "oxarbitrage@gmail.com",
            "name": "Alfredo Garcia",
            "username": "oxarbitrage"
          },
          "distinct": true,
          "id": "2872c31715dd6402718f94aa323dd1a4d35bd1f9",
          "message": "ci: revert to gh-pages branch for benchmark data storage",
          "timestamp": "2026-03-31T07:58:18-03:00",
          "tree_id": "bb3dea3409b947a53d9264b102ef7f3aa216b661",
          "url": "https://github.com/ZcashFoundation/zebra/commit/2872c31715dd6402718f94aa323dd1a4d35bd1f9"
        },
        "date": 1774956558250,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "Groth16 Verification/single proof",
            "value": 5759689.327777777,
            "range": "4078.2 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/2",
            "value": 11494684.76,
            "range": "5487.56 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/4",
            "value": 22997598.630000006,
            "range": "17524.46 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/8",
            "value": 46166393.735,
            "range": "262231.76 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/16",
            "value": 91996879.32,
            "range": "36174.95 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/32",
            "value": 183849559.76,
            "range": "65648.84 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/64",
            "value": 368003111.53,
            "range": "349778 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Input Preparation/primary_inputs",
            "value": 17258.50056227461,
            "range": "9.27 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Input Preparation/item_creation",
            "value": 626812.0863174228,
            "range": "475.74 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/single bundle",
            "value": 21400063.319999997,
            "range": "41371.13 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (4 actions)/2",
            "value": 42682181.295,
            "range": "78466.84 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (8 actions)/4",
            "value": 85443280.47,
            "range": "122050.25 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (16 actions)/8",
            "value": 171805401.22,
            "range": "187803.72 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (32 actions)/16",
            "value": 343180877.6,
            "range": "310988.32 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (64 actions)/32",
            "value": 686555752.8,
            "range": "1014174.12 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/single bundle",
            "value": 18489080.296666674,
            "range": "30967.16 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/2",
            "value": 27480662.805,
            "range": "13812.39 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/4",
            "value": 66644558.12,
            "range": "59797.75 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/8",
            "value": 123607315.21,
            "range": "46869.17 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/16",
            "value": 262363173.3,
            "range": "103879.86 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/32",
            "value": 540095794.01,
            "range": "604189.59 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/64",
            "value": 1069283647.16,
            "range": "618892.05 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/2",
            "value": 20877069.236666664,
            "range": "13056.6 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/4",
            "value": 35808546.185,
            "range": "16476.24 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/8",
            "value": 51170671.99,
            "range": "76831.43 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/16",
            "value": 81849756.32,
            "range": "200046.21 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/32",
            "value": 150576886.44,
            "range": "343000.64 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/64",
            "value": 269617014.32,
            "range": "1124602.99 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V1 transparent",
            "value": 753.0199816674501,
            "range": "0.71 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V2 sprout joinsplit",
            "value": 1284.9259747243018,
            "range": "1.31 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V3 overwinter",
            "value": 847.3587721724685,
            "range": "0.73 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V4 sapling",
            "value": 894.3917725658025,
            "range": "0.54 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V5 orchard",
            "value": 6302.760469481374,
            "range": "3.39 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V1 transparent",
            "value": 184.20857878445352,
            "range": "0.27 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V2 sprout joinsplit",
            "value": 407.82858872630715,
            "range": "5.92 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V3 overwinter",
            "value": 181.59636706086758,
            "range": "0.11 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V4 sapling",
            "value": 248.8738780462881,
            "range": "0.26 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V5 orchard",
            "value": 472.43114505457794,
            "range": "0.62 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/BLOCK_TESTNET_141042",
            "value": 161048.50850460105,
            "range": "135.19 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/BLOCK_TESTNET_141042",
            "value": 544326.4720209668,
            "range": "352.79 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/large_multi_transaction_block",
            "value": 1509830.461199767,
            "range": "769.22 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/large_multi_transaction_block",
            "value": 34001236.54000001,
            "range": "103635.43 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/large_single_transaction_block_many_inputs",
            "value": 1076349.739481654,
            "range": "1398.64 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/large_single_transaction_block_many_inputs",
            "value": 2296392.1871520095,
            "range": "1827.38 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/8",
            "value": 3468442.869333335,
            "range": "5026.54 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/8",
            "value": 672532.5222225506,
            "range": "174.93 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/16",
            "value": 6928735.83875,
            "range": "3941.03 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/16",
            "value": 1199482.6547775972,
            "range": "260.28 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/24",
            "value": 10417923.688000001,
            "range": "4476.87 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/24",
            "value": 1727317.2989005467,
            "range": "633.92 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/32",
            "value": 13872048.105,
            "range": "5275.55 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/32",
            "value": 2255439.853913044,
            "range": "1025.1 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/40",
            "value": 17354741.35666667,
            "range": "9511.29 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/40",
            "value": 2783521.989444444,
            "range": "1724.89 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/48",
            "value": 20826106.486666664,
            "range": "13604.4 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/48",
            "value": 3311142.35625,
            "range": "2144.08 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/56",
            "value": 24277402.859999992,
            "range": "12125.99 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/56",
            "value": 3833117.9521428575,
            "range": "1518.45 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/64",
            "value": 27757635.375,
            "range": "10119.46 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/64",
            "value": 4360181.317500001,
            "range": "2504.23 ns",
            "unit": "ns"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "oxarbitrage@gmail.com",
            "name": "Alfredo Garcia",
            "username": "oxarbitrage"
          },
          "committer": {
            "email": "oxarbitrage@gmail.com",
            "name": "Alfredo Garcia",
            "username": "oxarbitrage"
          },
          "distinct": true,
          "id": "d0b48c09316636b0471715934382a8fa97cba970",
          "message": "ci: add on-demand benchmark workflow with github-action-benchmark\n\nAdds a workflow_dispatch workflow that runs the full benchmark suite\nusing cargo-criterion and stores results via github-action-benchmark\non the gh-pages branch for historical tracking.\n\nFeatures:\n- Selective benchmarks via comma-separated input (or 'all')\n- Configurable regression alert threshold\n- Step summary table visible in the Actions UI\n- Converts cargo-criterion JSON to customSmallerIsBetter format",
          "timestamp": "2026-03-31T10:09:29-03:00",
          "tree_id": "bb3dea3409b947a53d9264b102ef7f3aa216b661",
          "url": "https://github.com/ZcashFoundation/zebra/commit/d0b48c09316636b0471715934382a8fa97cba970"
        },
        "date": 1774964466430,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "Groth16 Verification/single proof",
            "value": 5746879.06222222,
            "range": "2076.89 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/2",
            "value": 11504395.103999998,
            "range": "7725.1 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/4",
            "value": 23075703.880000006,
            "range": "122416 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/8",
            "value": 45980322.315,
            "range": "10853.44 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/16",
            "value": 91961314.45,
            "range": "18575.26 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/32",
            "value": 184045717.23,
            "range": "73717.96 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Verification/unbatched/64",
            "value": 367808647,
            "range": "83452.2 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Input Preparation/primary_inputs",
            "value": 17518.3179991643,
            "range": "62.31 ns",
            "unit": "ns"
          },
          {
            "name": "Groth16 Input Preparation/item_creation",
            "value": 626005.0212959952,
            "range": "207.12 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/single bundle",
            "value": 21629928.993333336,
            "range": "45833.32 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (4 actions)/2",
            "value": 43397708.585,
            "range": "62850.04 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (8 actions)/4",
            "value": 86215416.18,
            "range": "125489.37 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (16 actions)/8",
            "value": 173807898.45,
            "range": "223366.36 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (32 actions)/16",
            "value": 347881153.17,
            "range": "540994.96 ns",
            "unit": "ns"
          },
          {
            "name": "Halo2 Verification/unbatched (64 actions)/32",
            "value": 695497123.67,
            "range": "942790.61 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/single bundle",
            "value": 18587526.899999995,
            "range": "107761.88 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/2",
            "value": 27499145.44,
            "range": "25837.39 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/4",
            "value": 66678157.55,
            "range": "18742.1 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/8",
            "value": 123535081.4,
            "range": "51787.06 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/16",
            "value": 262192745.92,
            "range": "68721.57 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/32",
            "value": 539807353.02,
            "range": "587789.62 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/unbatched/64",
            "value": 1069670830.24,
            "range": "786259.24 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/2",
            "value": 20901973.18333334,
            "range": "5477.24 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/4",
            "value": 35850598.76,
            "range": "7660.22 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/8",
            "value": 51063997.59,
            "range": "135697.09 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/16",
            "value": 81930907.06,
            "range": "221905.6 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/32",
            "value": 150622276.27,
            "range": "303193.11 ns",
            "unit": "ns"
          },
          {
            "name": "Sapling Verification/batched/64",
            "value": 270235204.35,
            "range": "1283545.93 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V1 transparent",
            "value": 761.84462204637,
            "range": "0.28 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V2 sprout joinsplit",
            "value": 1277.6159929279722,
            "range": "1.14 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V3 overwinter",
            "value": 878.2312828526505,
            "range": "0.41 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V4 sapling",
            "value": 921.5772347762334,
            "range": "0.41 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Deserialization/deserialize/V5 orchard",
            "value": 6196.32789568942,
            "range": "3.92 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V1 transparent",
            "value": 206.5670557320345,
            "range": "0.14 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V2 sprout joinsplit",
            "value": 432.0926712339389,
            "range": "0.35 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V3 overwinter",
            "value": 228.13804168510154,
            "range": "0.23 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V4 sapling",
            "value": 239.03823725113585,
            "range": "0.1 ns",
            "unit": "ns"
          },
          {
            "name": "Transaction Serialization/serialize/V5 orchard",
            "value": 497.47393730572287,
            "range": "0.25 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/BLOCK_TESTNET_141042",
            "value": 181581.68655053742,
            "range": "330.33 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/BLOCK_TESTNET_141042",
            "value": 535675.1095544555,
            "range": "229.19 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/large_multi_transaction_block",
            "value": 2344141.425730926,
            "range": "2226.24 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/large_multi_transaction_block",
            "value": 32666890.765,
            "range": "263121.12 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_serialize_to_vec/large_single_transaction_block_many_inputs",
            "value": 1287165.2176878275,
            "range": "842.26 ns",
            "unit": "ns"
          },
          {
            "name": "zcash_deserialize/large_single_transaction_block_many_inputs",
            "value": 2287551.85613279,
            "range": "738.17 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/8",
            "value": 3463978.3733333354,
            "range": "6598.01 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/8",
            "value": 674326.9487409487,
            "range": "2920.47 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/16",
            "value": 6927077.625,
            "range": "3258.3 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/16",
            "value": 1199363.0388237033,
            "range": "423.18 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/24",
            "value": 10394229.564000001,
            "range": "4663.59 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/24",
            "value": 1726436.5168553274,
            "range": "536.33 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/32",
            "value": 13847611.3225,
            "range": "4513.93 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/32",
            "value": 2260701.1121739135,
            "range": "9505.28 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/40",
            "value": 17318543.213333335,
            "range": "25760.93 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/40",
            "value": 2780050.262631578,
            "range": "1099.18 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/48",
            "value": 20761524.906666666,
            "range": "4829.1 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/48",
            "value": 3306876.165625,
            "range": "770.47 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/56",
            "value": 24237449.096666664,
            "range": "16416.6 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/56",
            "value": 3834586.2650000006,
            "range": "2129.51 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Unbatched verification/64",
            "value": 27690489.71,
            "range": "10433.19 ns",
            "unit": "ns"
          },
          {
            "name": "Batch Verification/Batched verification/64",
            "value": 4361154.436666665,
            "range": "2271.57 ns",
            "unit": "ns"
          }
        ]
      }
    ]
  }
}