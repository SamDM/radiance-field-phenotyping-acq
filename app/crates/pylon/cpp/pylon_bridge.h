#pragma once
#include "rust/cxx.h"
#include <memory>
#include <cstdint>
#include <cstddef>
#include <string>

namespace rfp {

struct CameraArray;
struct Camera;
struct DeviceInfo;
struct GrabResult;

void pylon_init();
void pylon_terminate();

// Enumerates attached devices, attaches the first `n` to the returned array.
std::unique_ptr<CameraArray> new_camera_array(size_t n);

struct DeviceInfo {
    ~DeviceInfo();

    rust::String serial_number() const;
    rust::String ip_address() const;
    rust::String subnet_mask() const;
    rust::String friendly_name() const;

    struct Impl;
    std::unique_ptr<Impl> m_impl;

    explicit DeviceInfo(std::unique_ptr<Impl> impl);
};

struct Camera {
    ~Camera();

    void set_feature(rust::Str name, rust::Str value);
    rust::String get_feature(rust::Str name) const;
    void execute_command(rust::Str name);
    void load_config_file(rust::Str path);
    void save_config_file(rust::Str path);
    std::unique_ptr<DeviceInfo> device_info() const;

    struct Impl;
    std::unique_ptr<Impl> m_impl;

    explicit Camera(std::unique_ptr<Impl> impl);
};

struct CameraArray {
    CameraArray();
    ~CameraArray();

    void open();
    void close();
    void start_grabbing();
    void stop_grabbing();
    bool is_grabbing() const;

    size_t len() const;
    std::unique_ptr<Camera> get_camera(size_t index) const;

    // Returns a null UniquePtr on timeout or no result available.
    std::unique_ptr<GrabResult> retrieve_result(uint32_t timeout_ms);

    struct Impl;
    std::unique_ptr<Impl> m_impl;

    friend std::unique_ptr<CameraArray> new_camera_array(size_t n);
};

struct GrabResult {
    ~GrabResult();

    bool succeeded() const;
    rust::String error_description() const;
    uint32_t width() const;
    uint32_t height() const;
    size_t camera_index() const;
    // Slice lifetime is tied to this GrabResult (CGrabResultPtr keeps buffer alive).
    rust::Slice<const uint8_t> image_data() const;
    uint64_t timestamp_ns() const;
    rust::String pixel_format() const;

    struct Impl;
    std::unique_ptr<Impl> m_impl;

    explicit GrabResult(std::unique_ptr<Impl> impl);
    friend class CameraArray;
};

} // namespace rfp
