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
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_0.json \
    --output-path renders/pre-processed/crop_0/2025-09-29-11-09-46_crop_0.mp4
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_1.json \
    --output-path renders/pre-processed/crop_1/2025-09-29-11-09-46_crop_1.mp4
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_2.json \
    --output-path renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2.mp4
```

images
```
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_2.json \
    --output-path renders/pre-processed/crop_2/images \
    --output-format images
```

depth
```
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_2.json \
    --rendered-output-names depth \
    --depth-near-plane 0.1 \
    --depth-far-plane 0.5 \
    --colormap-options.colormap turbo \
    --colormap-options.invert True \
    --output-path renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2_depth.mp4
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_2.json \
    --rendered-output-names rgb depth \
    --depth-near-plane 0.1 \
    --depth-far-plane 0.5 \
    --colormap-options.colormap turbo \
    --colormap-options.invert True \
    --output-path renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2_rgb-depth.mp4
```

nearest cam (doesn't work)
```
ns-render camera-path \
    --load-config outputs/pre-processed/splatfacto/2025-09-29_114726/config.yml \
    --camera-path-filename pre-processed/camera_paths/2025-09-29-11-09-46.crop_2.json \
    --render-nearest-camera True \
    --output-path renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2_nearest.mp4
```

make gifs
```
(
  VID_NAME="2025-09-29-11-09-46_crop_0"
  DIR_NAME="crop_0"
  ffmpeg -i "renders/pre-processed/$DIR_NAME/$VID_NAME.mp4" -f yuv4mpegpipe - | \
  gifski -o "renders/pre-processed/$DIR_NAME/$VID_NAME.gif" -
)
(
  VID_NAME="2025-09-29-11-09-46_crop_1"
  DIR_NAME="crop_1"
  ffmpeg -i "renders/pre-processed/$DIR_NAME/$VID_NAME.mp4" -f yuv4mpegpipe - | \
  gifski -o "renders/pre-processed/$DIR_NAME/$VID_NAME.gif" -
)
(
  VID_NAME="2025-09-29-11-09-46_crop_2"
  DIR_NAME="crop_2"
  ffmpeg -i "renders/pre-processed/$DIR_NAME/$VID_NAME.mp4" -f yuv4mpegpipe - | \
  gifski -o "renders/pre-processed/$DIR_NAME/$VID_NAME.gif" -
)
(
  VID_NAME="2025-09-29-11-09-46_crop_2_depth"
  DIR_NAME="crop_2"
  ffmpeg -i "renders/pre-processed/$DIR_NAME/$VID_NAME.mp4" -f yuv4mpegpipe - | \
  gifski -o "renders/pre-processed/$DIR_NAME/$VID_NAME.gif" -
)
(
  VID_NAME="2025-09-29-11-09-46_crop_2_rgb-depth"
  DIR_NAME="crop_2"
  ffmpeg -i "renders/pre-processed/$DIR_NAME/$VID_NAME.mp4" -f yuv4mpegpipe - | \
  gifski -o "renders/pre-processed/$DIR_NAME/$VID_NAME.gif" -
)
```

Concat videos
```
ffmpeg \
    -f concat \
    -safe 0 \
    -i <(printf "file '%s/%s'\n" \
        "$PWD" renders/pre-processed/crop_0/2025-09-29-11-09-46_crop_0.mp4 \
        "$PWD" renders/pre-processed/crop_1/2025-09-29-11-09-46_crop_1.mp4 \
        "$PWD" renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2.mp4 \
       ) \
    -c copy \
    renders/pre-processed/concat.mp4

ffmpeg -i renders/pre-processed/concat.mp4 -f yuv4mpegpipe - | gifski -o renders/pre-processed/concat.gif -

ffmpeg \
    -f concat \
    -safe 0 \
    -i <(printf "file '%s/%s'\n" \
        "$PWD" renders/pre-processed/crop_0/2025-09-29-11-09-46_crop_0.mp4 \
        "$PWD" renders/pre-processed/crop_1/2025-09-29-11-09-46_crop_1.mp4 \
        "$PWD" renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2.mp4 \
        "$PWD" renders/pre-processed/crop_2/2025-09-29-11-09-46_crop_2_depth.mp4 \
       ) \
    -c copy \
    renders/pre-processed/concat_depth.mp4

ffmpeg -i renders/pre-processed/concat_depth.mp4 -f yuv4mpegpipe - | gifski -o renders/pre-processed/concat_depth.gif -
```

Transcode input movie:
```
ffmpeg -i CP-176_4.MOV -c:v libx264 -preset medium -crf 24 -c:a aac -b:a 128k CP-176_4.mp4
```

Live preview in NS
```
ffmpeg -i "Screencast 2025-09-30 14:23:33.mp4" -f yuv4mpegpipe - | gifski -W 900 --lossy-quality=50 -o "Screencast 2025-09-30 14:23:33.gif" -

```

# Plaatjes & Pintjes

```
export WORKDIR="/home/samey/Safe/Projects/radiance-field-phenotyping/data/plaatjesenpintjes"

ns-process-data images \
  --data "$WORKDIR/preproc-images" \
  --output-dir "$WORKDIR/preproc-ns"

ns-train splatfacto-big \
  --data "$WORKDIR/preproc-ns" \
  --output-dir "$WORKDIR/checkpoints-ns" \
  --downscale_factor=1

ns-export gaussian-splat \
  --load-config "$WORKDIR/checkpoints-ns/preproc-ns/splatfacto/2025-11-09_210223/config.yml" \
  --output-dir "$WORKDIR/exports-ns/splat_2025-11-09_210223" \
  --obb_center 0.0427723397 -0.0291634388 -0.2466881491 \
  --obb_rotation 0.0000000000 0.0000000000 0.0000000000 \
  --obb_scale 0.6100000143 0.4799999893 0.8000000119
```