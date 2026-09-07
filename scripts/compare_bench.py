import json
import os
import glob

def load_criterion_estimates(base_dir="target/criterion"):
    results = {}
    pattern = os.path.join(base_dir, "**", "estimates.json")
    for file_path in glob.glob(pattern, recursive=True):
        parts = file_path.split(os.sep)
        if len(parts) >= 2:
            bench_name = parts[-2]
            try:
                with open(file_path, "r") as f:
                    data = json.load(f)
                    mean_time = data.get("median", {}).get("point_estimate", 0)
                    results[bench_name] = mean_time
            except Exception as e:
                print(f"Erreur lecture {file_path}: {e}")
    return results

def compare_results(baseline, current, threshold_pct=2.0):
    print("\n📊 --- RAPPORT DE COMPARISON DES PERFORMANCES ---")
    print(f"{'Benchmark':<30} | {'Baseline (ns)':<15} | {'PR (ns)':<15} | {'Écart (%)':<10}")
    print("-" * 75)
    
    for bench, curr_val in current.items():
        base_val = baseline.get(bench, curr_val)
        if base_val == 0:
            diff_pct = 0.0
        else:
            diff_pct = ((curr_val - base_val) / base_val) * 100
        
        # Gestion du bruit / seuil de tolérance
        status = ""
        if diff_pct > threshold_pct:
            status = " ⚠️ Ralentissement"
        elif diff_pct < -threshold_pct:
            status = " 🚀 Amélioration"
        else:
            status = " ⚖️ Stable"

        print(f"{bench:<30} | {base_val:<15.2f} | {curr_val:<15.2f} | {diff_pct:+.2f}% {status}")

if __name__ == "__main__":
    print("🔍 Analyse des benchmarks Criterion locaux...")
    current_res = load_criterion_estimates()
    
    # Pour le test local, on simule une baseline identique ou légèrement modifiée
    baseline_res = current_res.copy() 
    
    compare_results(baseline_res, current_res)
