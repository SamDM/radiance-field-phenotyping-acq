The `--nvccli` seems to help for colmap, prevents mapping GLIBC files from the host (ubunut 24.04) to the container (ubuntu 22.04).

```
apptainer shell --nvccli nerfstudio_latest.sif
```

# Poster example

The `downscale-factor` is an argument to `colmap`, why? No clue, but it helps a lot in reconstruction quality.
```
ns-train splatfacto --data data/nerfstudio/poster colmap --downscale_factor=1
```

# Real video

Working dir
```
cd /home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4
```

```
ns-train splatfacto --data pre-processed colmap --downscale_factor=1
ns-train splatfacto-big --data pre-processed colmap --downscale_factor=1
```

```
ns-viewer --load-config outputs/pre-processed/splatfacto/2025-09-26_125020/config.yml
```

Export:
```
ns-export gaussian-splat --load-config outputs/pre-processed/splatfacto/2025-09-26_125020/config.yml --output-dir exports/splat/ --obb_center -0.0648839232 0.0350148000 -0.0483375749 --obb_rotation 0.0000000000 0.0000000000 -1.0438496510 --obb_scale 1.0000000000 1.6000000238 1.0000000000
```

Render (hand picked path):
```
ns-render camera-path --load-config outputs/pre-processed/splatfacto/2025-09-26_125020/config.yml --camera-path-filename /home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/pre-processed/camera_paths/2025-09-29-11-09-46.json --output-path renders/pre-processed/2025-09-29-11-09-46.mp4
```

Render data:

```
ns-render dataset --load-config outputs/pre-processed/splatfacto/2025-09-26_125020/config.yml --output-path renders/pre-processed/2025-09-29-11-09-46.data.mp4
```

# Real video, big:

```
ns-viewer --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml
```

Export:
```
ns-export gaussian-splat --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml --output-dir exports/splat/ --obb_center -0.0648839232 0.0350148000 -0.0483375749 --obb_rotation 0.0000000000 0.0000000000 -1.0438496510 --obb_scale 1.0000000000 1.6000000238 1.0000000000
```

```
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_1.json \
    --output-path renders/pre-processed/crop_1/2025-09-29-11-09-46_crop_1.mp4
```