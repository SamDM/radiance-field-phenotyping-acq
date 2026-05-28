## pylon crate: `CameraArray::get_camera` does not enforce mutability

`get_camera(&self, i) -> Camera<'_>` currently allows callers to mutate camera
hardware state (via `set_feature`, `execute_command`) even when holding a shared
`&CameraArray`. The mutation goes through a raw pointer inside `Camera<'_>`,
bypassing the borrow checker.

**Fix (Option A — collapses read/write distinction):**  
Change `get_camera` to take `&mut self`. One-line pylon change; all call sites
that currently pass `&CameraArray` and mutate must switch to `&mut CameraArray`.
Read-only sites (e.g. `get_camera_interfaces`) would also require `&mut`, which
is slightly noisy.

**Fix (Option B — full semantics):**  
Split into two methods and two types:
- `get_camera(&self, i) -> CameraRef<'_>` — exposes `get_feature`, `device_info` only  
- `get_camera_mut(&mut self, i) -> Camera<'_>` — exposes `set_feature`, `execute_command`, etc.

Update call sites: read-only uses → `get_camera`, mutable uses → `get_camera_mut`.
More churn but enforces the intended semantics at the type level.
