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
    camera_location: np.ndarray,
    look_at: np.ndarray | None = None,
    up: np.ndarray | None = None,
) -> np.ndarray:
    """
    Create a world-to-camera homogeneous transformation matrix.

    Args:
        camera_location: 3D position of the camera in world coordinates (x, y, z)
        look_at: 3D point the camera is looking at in world coordinates.
                 Defaults to origin (0, 0, 0)
        up: 3D vector indicating the "up" direction of the camera (points to -Y in camera frame).
            Defaults to world Z-axis (0, 0, 1) projected to be perpendicular to view direction.

    Returns:
        4x4 homogeneous matrix that transforms world coordinates to camera coordinates.
        Camera frame: +Z forward (look direction), +X right, +Y down
        World frame: +Z up

    Example:
        >>> cam_pos = np.array([5, 5, 3])
        >>> look = np.array([0, 0, 0])
        >>> T_wc = world_to_camera_matrix(cam_pos, look)
    """
    camera_location = np.asarray(camera_location, dtype=float)

    if look_at is None:
        look_at = np.array([0.0, 0.0, 0.0])
    else:
        look_at = np.asarray(look_at, dtype=float)

    # Camera Z-axis: forward direction (from camera to look_at point)
    z_cam_world = look_at - camera_location
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
        # If nearly parallel, choose a different reference vector
        if abs(world_up[0]) < 0.9:
            world_up = np.array([1.0, 0.0, 0.0])
        else:
            world_up = np.array([0.0, 1.0, 0.0])

    # Camera X-axis: right direction (perpendicular to both Z and up)
    # Right = forward × up (cross product)
    x_cam_world = np.cross(z_cam_world, world_up)
    x_cam_world = x_cam_world / np.linalg.norm(x_cam_world)

    # Camera Y-axis: down direction (perpendicular to both X and Z)
    # Down = right × forward
    y_cam_world = np.cross(x_cam_world, z_cam_world)

    # Build rotation matrix (world to camera)
    # Each row represents where a camera axis points in world coordinates
    R_wc = np.vstack([x_cam_world, y_cam_world, z_cam_world])

    # Translation: transform camera position to camera frame origin
    t_wc = -R_wc @ camera_location

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

        # fp = (hom_c2w @ [0, 0, 0, 1])[:3]
        # rr.log(f"p_{i}", rr.Points3D(positions=[[fp]], radii=[0.1]))

    # pcd = rr.Asset3D(path="/home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/exports/splat/splat.ply")
    rr.log_file_from_path(
        entity_path_prefix="pcd",
        file_path="/home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/exports/splat/splat.ply",
    )


def main():
    cam_wh = (1920, 1080)
    cam_f = 1740
    jsond = json.loads(
        Path(
            "/home/samey/Safe/Projects/radiance-field-phenotyping/data/CP-176_4/pre-processed/camera_paths/2025-09-29-11-09-46.pretty.json"
        ).read_text()
    )
    # homs_w2c = [
    #     np.linalg.inv(np.array(kf["matrix"]).reshape(4, 4) @ FLIP_MAT)
    #     for kf in jsond["keyframes"]
    # ]
    homs_w2c = [
        look_at([1, 0, 0]),
        look_at([0, 1, 0]),
        look_at([-1, 0, 0]),
        look_at([0, -1, 0]),
    ]
    show_in_rr(cam_wh, cam_f, homs_w2c)


if __name__ == "__main__":
    main()
