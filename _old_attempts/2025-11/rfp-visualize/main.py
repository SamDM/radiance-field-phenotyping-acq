import json
import numpy as np
import numpy.typing as npt
import rerun as rr
from pathlib import Path

FLIP_MAT = np.array(
    [
        [1, 0, 0, 0],  # rotates
        [0, -1, 0, 0],  # 180
        [0, 0, -1, 0],  # around
        [0, 0, 0, 1],  # x-axis
    ]
)


def look_at(
    at: npt.NDArray[np.floating],
    eye: npt.NDArray[np.floating],
    up: np.ndarray | None = None,
) -> np.ndarray:
    """
    Create a world-to-camera homogeneous transformation matrix.

    Args:
        at: 3D point the camera is looking at in world coordinates.
                 Defaults to origin (0, 0, 0)
        eye: 3D position of the camera in world coordinates (x, y, z)
        up: 3D vector indicating the "up" direction of the camera (points to -Y in camera frame).
            Defaults to world Z-axis (0, 0, 1) projected to be perpendicular to view direction.

    Returns:
        4x4 homogeneous matrix that transforms world coordinates to camera coordinates.
        Camera frame: +Z forward (look direction), +X right, +Y down
        World frame: +Z up
    """
    eye = np.asarray(eye, dtype=float)

    if at is None:
        at = np.array([0.0, 0.0, 0.0])
    else:
        at = np.asarray(at, dtype=float)

    # Camera Z-axis: forward direction (from camera to look_at point)
    z_cam_world = at - eye
    z_cam_world_norm = np.linalg.norm(z_cam_world)

    if z_cam_world_norm < 1e-10:
        raise ValueError("Camera location and look_at point are too close")

    z_cam_world = z_cam_world / z_cam_world_norm

    # Determine the up direction
    if up is None:
        # Default: use world Z-axis (0, 0, 1) as reference for "up"
        world_up = np.array([0.0, 0.0, 1.0])
    else:
        world_up = np.asarray(up, dtype=float)
        world_up_norm = np.linalg.norm(world_up)
        if world_up_norm < 1e-10:
            raise ValueError("Up vector has zero length")
        world_up = world_up / world_up_norm

    # Check if look direction is parallel to up direction
    if abs(np.dot(z_cam_world, world_up)) > 0.9999:
        raise Exception("Up direction and looking direction are parallel")

    # Camera X-axis: right direction (perpendicular to both Z and up)
    # Right = forward × up (cross product)
    x_cam_world = np.cross(z_cam_world, world_up)
    x_cam_world = x_cam_world / np.linalg.norm(x_cam_world)

    # Camera Y-axis: down direction (perpendicular to both X and Z)
    # Down = forward × right
    y_cam_world = np.cross(z_cam_world, x_cam_world)

    # Build rotation matrix (world to camera)
    # Each row represents where a camera axis points in world coordinates
    R_wc = np.vstack([x_cam_world, y_cam_world, z_cam_world])

    # Translation: transform camera position to camera frame origin
    t_wc = -R_wc @ eye

    # Build 4x4 homogeneous matrix
    T_wc = np.eye(4)
    T_wc[:3, :3] = R_wc
    T_wc[:3, 3] = t_wc

    return T_wc


def show_in_rr(
    cam_wh: tuple[int, int], cam_f: float, homs_w2c: list[npt.NDArray[np.floating]]
):
    rr.init("show_camera_trajectory", spawn=True)
    rr.log(
        "origin",
        rr.Arrows3D(
            vectors=[[1, 0, 0], [0, 1, 0], [0, 0, 1]],
            colors=[[255, 0, 0], [0, 255, 0], [0, 0, 255]],
        ),
    )

    w, h = cam_wh
    rr_cam = rr.Pinhole(width=w, height=h, focal_length=cam_f)
    for i, hom_w2c in enumerate(homs_w2c):
        hom_c2w = np.linalg.inv(hom_w2c)
        rr_tf = rr.Transform3D(mat3x3=hom_c2w[:3, :3], translation=hom_c2w[:3, 3])
        rr.log(f"cam_{i}", rr_cam)
        rr.log(f"cam_{i}", rr_tf)

    rr.log_file_from_path(
        entity_path_prefix="pcd",
        file_path="/home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/exports/splat/splat.ply",
    )


def generate_camera_trajectory() -> list[npt.NDArray[np.floating]]:
    fps = 30
    secs = 3
    n = fps * secs

    z = -0.05
    dist = 0.35
    center = np.array([-0.06, 0.04, -0.06])

    hom_translate_center = np.eye(4)
    hom_translate_center[:3, 3] = -center
    homs_w2c = []
    for i in range(n):
        frac_n = i / n
        frac_rad = 2 * np.pi * frac_n
        x = np.cos(frac_rad) * dist
        y = np.sin(frac_rad) * dist
        hom_w2c = look_at((0, 0, 0), (x, y, z)) @ hom_translate_center
        homs_w2c.append(hom_w2c)
    return homs_w2c


def list_crops() -> list:
    crop_0 = None
    crop_1 = dict(
        crop_center=[-0.0648839232, 0.0350148000, -0.0483375749],
        crop_scale=[1.0, 1.6, 1.0],
        crop_rot=[0.0, 0.0, -1.0438496510],
        crop_bg_color={"r": 38, "g": 42, "b": 55},
    )
    crop_2 = dict(
        crop_center=[-0.0648839232, 0.0350148000, -0.0483375749],
        crop_scale=[0.6, 0.6, 0.6],
        crop_rot=[0.0, 0.0, -1.0438496510],
        crop_bg_color={"r": 38, "g": 42, "b": 55},
    )

    return [crop_0, crop_1, crop_2]


def main():
    work_dpath = Path(
        "/home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/pre-processed/camera_paths/"
    )
    jsond: dict = json.loads(
        (work_dpath / "2025-09-29-11-09-46.pretty.json").read_text()
    )
    # homs_w2c = [
    #     np.linalg.inv(np.array(kf["matrix"]).reshape(4, 4) @ FLIP_MAT)
    #     for kf in jsond["keyframes"]
    # ]

    homs_w2c = generate_camera_trajectory()
    # cam_wh = (1920, 1080)
    # cam_f = 1740
    # show_in_rr(cam_wh, cam_f, homs_w2c)

    example: dict = jsond["camera_path"][0]
    new_camera_path = []
    for hom_w2c in homs_w2c:
        hom_ns = np.linalg.inv(hom_w2c) @ np.linalg.inv(FLIP_MAT)
        new_entry = example.copy()
        new_entry["camera_to_world"] = hom_ns.flatten().tolist()
        new_camera_path.append(new_entry)
    new_jsond = jsond.copy()
    new_jsond["camera_path"] = new_camera_path
    new_jsond["seconds"] = 3.0

    for i, crop in enumerate(list_crops()):
        new_jsond["crop"] = crop
        (work_dpath / f"2025-09-29-11-09-46.crop_{i}.json").write_text(
            json.dumps(new_jsond, indent=2)
        )


if __name__ == "__main__":
    main()
