window.BENCHMARK_DATA = {
  "lastUpdate": 1788106504867,
  "repoUrl": "https://github.com/sidux/phpantom_lsp",
  "entries": {
    "PHPantom Benchmarks": [
      {
        "commit": {
          "author": {
            "email": "sidux@users.noreply.github.com",
            "name": "sidux",
            "username": "sidux"
          },
          "committer": {
            "email": "sidux@users.noreply.github.com",
            "name": "sidux",
            "username": "sidux"
          },
          "distinct": false,
          "id": "199b79171ab42c8ec95d924c5b97fc89f705e3bb",
          "message": "feat(lsp): add configurable framework metadata\n\nAdd generic YAML/XML PHP navigation, scalable reference lenses, semantic indexing, call hierarchy, transparent proxy metadata, and config-driven Symfony event and ExpressionLanguage adapters.",
          "timestamp": "2026-08-30T17:03:23+02:00",
          "tree_id": "ee432e25154751b8d54649b05b5e9044d4126437",
          "url": "https://github.com/sidux/phpantom_lsp/commit/199b79171ab42c8ec95d924c5b97fc89f705e3bb"
        },
        "date": 1788102953173,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_start_completion",
            "value": 4.462,
            "range": "± 0.295",
            "unit": "ms"
          },
          {
            "name": "completion_simple_class",
            "value": 0.038,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_5",
            "value": 0.105,
            "range": "± 0.007",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_10",
            "value": 0.154,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_20",
            "value": 0.251,
            "range": "± 0.003",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/100_classes",
            "value": 0.299,
            "range": "± 0.006",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/500_classes",
            "value": 1.293,
            "range": "± 0.054",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/1000_classes",
            "value": 2.508,
            "range": "± 0.053",
            "unit": "ms"
          },
          {
            "name": "completion_generics_and_mixins",
            "value": 0.123,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "completion_with_narrowing",
            "value": 0.051,
            "range": "± 0.002",
            "unit": "ms"
          },
          {
            "name": "completion_5_method_chain",
            "value": 0.046,
            "range": "± 0.002",
            "unit": "ms"
          },
          {
            "name": "completion_cross_file_type_hint",
            "value": 0.054,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_carbon_class",
            "value": 6.698,
            "range": "± 0.123",
            "unit": "ms"
          },
          {
            "name": "completion_yii_deep_hierarchy",
            "value": 0.188,
            "range": "± 0.012",
            "unit": "ms"
          },
          {
            "name": "completion_large_file",
            "value": 0.356,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "completion_short_file",
            "value": 0.062,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "variable_completion/short",
            "value": 0.04,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "variable_completion/long",
            "value": 0.117,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "hover_method_call",
            "value": 0.101,
            "range": "± 0.006",
            "unit": "ms"
          },
          {
            "name": "goto_definition_method",
            "value": 0.083,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/100_lines",
            "value": 0.211,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/500_lines",
            "value": 1.135,
            "range": "± 0.039",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/2000_lines",
            "value": 6.453,
            "range": "± 0.296",
            "unit": "ms"
          },
          {
            "name": "reparse_500_line_file",
            "value": 1.149,
            "range": "± 0.034",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_generic_objects",
            "value": 0.036,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_objects",
            "value": 0.035,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_missing_methods",
            "value": 83.494,
            "range": "± 2.185",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/method_chain",
            "value": 2.511,
            "range": "± 0.079",
            "unit": "ms"
          }
        ]
      },
      {
        "commit": {
          "author": {
            "email": "sidux@users.noreply.github.com",
            "name": "sidux",
            "username": "sidux"
          },
          "committer": {
            "email": "sidux@users.noreply.github.com",
            "name": "sidux",
            "username": "sidux"
          },
          "distinct": true,
          "id": "f4925609e75d25536c1008014aa9cbb6451dcafd",
          "message": "fix(release): support unsigned macOS builds\n\nKeep signing and notarization enabled when the complete Apple credential set is available. Package unsigned macOS artifacts when credentials are absent so release workflows remain usable from forks.",
          "timestamp": "2026-08-30T18:01:36+02:00",
          "tree_id": "2b240679816042feb81bbc6bebef64f24e0922e1",
          "url": "https://github.com/sidux/phpantom_lsp/commit/f4925609e75d25536c1008014aa9cbb6451dcafd"
        },
        "date": 1788106504270,
        "tool": "customSmallerIsBetter",
        "benches": [
          {
            "name": "cold_start_completion",
            "value": 4.372,
            "range": "± 0.062",
            "unit": "ms"
          },
          {
            "name": "completion_simple_class",
            "value": 0.039,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_5",
            "value": 0.105,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_10",
            "value": 0.155,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_inheritance_depth/depth_20",
            "value": 0.252,
            "range": "± 0.006",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/100_classes",
            "value": 0.3,
            "range": "± 0.016",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/500_classes",
            "value": 1.297,
            "range": "± 0.013",
            "unit": "ms"
          },
          {
            "name": "completion_classmap_size/1000_classes",
            "value": 2.528,
            "range": "± 0.016",
            "unit": "ms"
          },
          {
            "name": "completion_generics_and_mixins",
            "value": 0.115,
            "range": "± 0.008",
            "unit": "ms"
          },
          {
            "name": "completion_with_narrowing",
            "value": 0.052,
            "range": "± 0.002",
            "unit": "ms"
          },
          {
            "name": "completion_5_method_chain",
            "value": 0.047,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "completion_cross_file_type_hint",
            "value": 0.052,
            "range": "± 0.003",
            "unit": "ms"
          },
          {
            "name": "completion_carbon_class",
            "value": 6.67,
            "range": "± 0.028",
            "unit": "ms"
          },
          {
            "name": "completion_yii_deep_hierarchy",
            "value": 0.181,
            "range": "± 0.017",
            "unit": "ms"
          },
          {
            "name": "completion_large_file",
            "value": 0.359,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "completion_short_file",
            "value": 0.061,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "variable_completion/short",
            "value": 0.041,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "variable_completion/long",
            "value": 0.116,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "hover_method_call",
            "value": 0.101,
            "range": "± 0.005",
            "unit": "ms"
          },
          {
            "name": "goto_definition_method",
            "value": 0.083,
            "range": "± 0.004",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/100_lines",
            "value": 0.213,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/500_lines",
            "value": 1.132,
            "range": "± 0.031",
            "unit": "ms"
          },
          {
            "name": "update_ast_parse_time/2000_lines",
            "value": 5.985,
            "range": "± 0.045",
            "unit": "ms"
          },
          {
            "name": "reparse_500_line_file",
            "value": 1.142,
            "range": "± 0.021",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_generic_objects",
            "value": 0.036,
            "range": "± 0",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_new_objects",
            "value": 0.035,
            "range": "± 0.001",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/lots_of_missing_methods",
            "value": 82.955,
            "range": "± 0.748",
            "unit": "ms"
          },
          {
            "name": "diagnostics/fixture/method_chain",
            "value": 2.451,
            "range": "± 0.024",
            "unit": "ms"
          }
        ]
      }
    ]
  }
}