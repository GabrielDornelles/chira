"""Export every candidate detector to fp16 ONNX at 544x960 (dynamic batch). usage: export_all.py WORKDIR family name..."""
import os, shutil, sys
work, family, names = sys.argv[1], sys.argv[2], sys.argv[3:]
os.chdir(work + "/pt")
out = work + "/onnx"
for name in names:
    try:
        if family == "ultralytics":
            from ultralytics import YOLO
            p = YOLO(name + ".pt").export(format="onnx", imgsz=(544, 960), half=True, dynamic=True, simplify=True, device=0, opset=17)
            shutil.move(p, f"{out}/{name}.onnx")
        elif family == "rfdetr":
            import rfdetr
            cls = {"rfdetr_n": rfdetr.RFDETRNano, "rfdetr_s": rfdetr.RFDETRSmall, "rfdetr_m": rfdetr.RFDETRMedium, "rfdetr_l": rfdetr.RFDETRLarge}[name]
            m = cls()
            p = m.export(output_dir=work + "/pt/" + name, shape=(544, 960), dynamic_batch=True, fp16=True)
            shutil.move(str(p), f"{out}/{name}.onnx")
        print("EXPORTED", name, flush=True)
    except Exception as e:
        import traceback; traceback.print_exc()
        print("FAILED", name, e, flush=True)
