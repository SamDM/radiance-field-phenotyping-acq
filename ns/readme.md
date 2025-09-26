The `--nvccli` seems to help for colmap, prevents mapping GLIBC files from the host (ubunut 24.04) to the container (ubuntu 22.04).

```
apptainer shell --nvccli nerfstudio_latest.sif
```

The `downscale-factor` is an argument to `colmap`, why? No clue, but it helps a lot in reconstruction quality.
```
ns-train splatfacto --data data/nerfstudio/poster colmap --downscale_factor=1
```