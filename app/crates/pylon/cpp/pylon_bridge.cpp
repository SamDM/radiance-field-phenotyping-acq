#include "pylon_bridge.h"

#include <pylon/PylonIncludes.h>
#include <pylon/InstantCameraArray.h>
#include <pylon/FeaturePersistence.h>

#include <GenApi/GenApi.h>

#include <stdexcept>
#include <string>

namespace rfp {

// ── Impl definitions ─────────────────────────────────────────────────────────

struct CameraArray::Impl {
    Pylon::CInstantCameraArray cameras;
};

struct Camera::Impl {
    Pylon::CInstantCamera* camera; // non-owning, stable pointer into CInstantCameraArray
};

struct DeviceInfo::Impl {
    Pylon::CDeviceInfo info;
    explicit Impl(const Pylon::CDeviceInfo& di) : info(di) {}
};

struct GrabResult::Impl {
    Pylon::CGrabResultPtr ptr;
    uint64_t timestamp_ns = 0;
};

// ── DeviceInfo ───────────────────────────────────────────────────────────────

DeviceInfo::DeviceInfo(std::unique_ptr<Impl> impl) : m_impl(std::move(impl)) {}
DeviceInfo::~DeviceInfo() = default;

rust::String DeviceInfo::serial_number() const {
    return rust::String(m_impl->info.GetSerialNumber().c_str());
}

rust::String DeviceInfo::ip_address() const {
    return rust::String(m_impl->info.GetIpAddress().c_str());
}

rust::String DeviceInfo::subnet_mask() const {
    return rust::String(m_impl->info.GetSubnetMask().c_str());
}

rust::String DeviceInfo::friendly_name() const {
    return rust::String(m_impl->info.GetFriendlyName().c_str());
}

// ── Pylon lifecycle ──────────────────────────────────────────────────────────

void pylon_init() {
    Pylon::PylonInitialize();
}

void pylon_terminate() {
    Pylon::PylonTerminate();
}

// ── CameraArray ──────────────────────────────────────────────────────────────

CameraArray::CameraArray() : m_impl(std::make_unique<Impl>()) {}
CameraArray::~CameraArray() {
    // TODO: this looks like a its gonna cause me problems.
    try { m_impl->cameras.StopGrabbing(); } catch (...) {}
    try { m_impl->cameras.Close(); } catch (...) {}
}

std::unique_ptr<CameraArray> new_camera_array(size_t n) {
    if (n == 0) {
        throw std::invalid_argument("camera count must be > 0");
    }

    Pylon::DeviceInfoList_t devices;
    size_t found = Pylon::CTlFactory::GetInstance().EnumerateDevices(devices);
    if (found < n) {
        throw std::runtime_error(
            "requested " + std::to_string(n) +
            " cameras but only found " + std::to_string(found));
    }

    auto arr = std::make_unique<CameraArray>();
    arr->m_impl->cameras.Initialize(n);
    for (size_t i = 0; i < n; ++i) {
        arr->m_impl->cameras[i].Attach(
            Pylon::CTlFactory::GetInstance().CreateDevice(devices[i]));
        arr->m_impl->cameras[i].SetCameraContext(static_cast<intptr_t>(i));
    }
    return arr;
}

void CameraArray::open() {
    m_impl->cameras.Open();
}

void CameraArray::close() {
    m_impl->cameras.Close();
}

void CameraArray::start_grabbing() {
    m_impl->cameras.StartGrabbing(
        Pylon::GrabStrategy_OneByOne,
        Pylon::GrabLoop_ProvidedByUser);
}

void CameraArray::stop_grabbing() {
    m_impl->cameras.StopGrabbing();
}

bool CameraArray::is_grabbing() const {
    return m_impl->cameras.IsGrabbing();
}

size_t CameraArray::len() const {
    return m_impl->cameras.GetSize();
}

std::unique_ptr<Camera> CameraArray::get_camera(size_t idx) const {
    if (idx >= m_impl->cameras.GetSize())
        throw std::out_of_range("camera index " + std::to_string(idx) + " out of range");
    auto impl = std::make_unique<Camera::Impl>();
    impl->camera = &m_impl->cameras[idx];
    return std::make_unique<Camera>(std::move(impl));
}

std::unique_ptr<GrabResult> CameraArray::retrieve_result(uint32_t timeout_ms) {
    auto impl = std::make_unique<GrabResult::Impl>();
    bool ok = m_impl->cameras.RetrieveResult(
        timeout_ms,
        impl->ptr,
        Pylon::TimeoutHandling_Return);
    if (!ok || !impl->ptr.IsValid()) {
        return nullptr;
    }

    // Compute timestamp in nanoseconds
    auto camera_idx = static_cast<size_t>(impl->ptr->GetCameraContext());
    if (camera_idx < m_impl->cameras.GetSize()) {
        auto& camera = m_impl->cameras[camera_idx];
        GenApi::CIntegerPtr tick_freq_node(camera.GetNodeMap().GetNode("GevTimestampTickFrequency"));
        if (tick_freq_node.IsValid()) {
            uint64_t tick_freq = tick_freq_node->GetValue();
            uint64_t ticks = impl->ptr->GetTimeStamp();
            impl->timestamp_ns = static_cast<uint64_t>((double)ticks * 1e9 / tick_freq);
        }
    }

    return std::unique_ptr<GrabResult>(new GrabResult(std::move(impl)));
}

// ── Camera ───────────────────────────────────────────────────────────────────

Camera::Camera(std::unique_ptr<Impl> impl) : m_impl(std::move(impl)) {}
Camera::~Camera() = default;

void Camera::set_feature(rust::Str name, rust::Str value) {
    std::string n(name), v(value);
    std::string exc_msg;
    {
        // Nested scope ensures val is destroyed before we throw, preventing
        // a double-throw if CValuePtr's destructor also throws.
        GenApi::CValuePtr val(m_impl->camera->GetNodeMap().GetNode(n.c_str()));
        if (!val.IsValid()) {
            exc_msg = "unknown or non-value feature: " + n;
        } else {
            try {
                val->FromString(v.c_str());
            } catch (const std::exception& e) {
                exc_msg = "feature '" + n + "' = '" + v + "': " + e.what();
            } catch (...) {
                exc_msg = "feature '" + n + "' = '" + v + "': unknown exception";
            }
        }
    }
    if (!exc_msg.empty()) throw std::runtime_error(exc_msg);
}

rust::String Camera::get_feature(rust::Str name) const {
    std::string n(name);
    std::string exc_msg;
    std::string result;
    {
        // Nested scope: see set_feature. Reading a node that is merely unavailable
        // (e.g. PtpStatus while PTP is off) throws, and CValuePtr's destructor can
        // throw too — unwinding through both aborts the process.
        GenApi::CValuePtr val(m_impl->camera->GetNodeMap().GetNode(n.c_str()));
        if (!val.IsValid()) {
            exc_msg = "unknown or non-value feature: " + n;
        } else {
            try {
                result = val->ToString().c_str();
            } catch (const std::exception& e) {
                exc_msg = "reading feature '" + n + "': " + e.what();
            } catch (...) {
                exc_msg = "reading feature '" + n + "': unknown exception";
            }
        }
    }
    if (!exc_msg.empty()) throw std::runtime_error(exc_msg);
    return rust::String(result);
}

void Camera::execute_command(rust::Str name) {
    std::string n(name);
    std::string exc_msg;
    {
        // Nested scope: see set_feature. Executing a command that is unavailable in
        // the current camera state (e.g. PtpDataSetLatch while PTP is off) throws,
        // and CCommandPtr's destructor can throw too — without this scope the two
        // unwind together and std::terminate aborts the process.
        GenApi::CCommandPtr cmd(m_impl->camera->GetNodeMap().GetNode(n.c_str()));
        if (!cmd.IsValid()) {
            exc_msg = "not a command node: " + n;
        } else {
            try {
                cmd->Execute();
            } catch (const std::exception& e) {
                exc_msg = "executing command '" + n + "': " + e.what();
            } catch (...) {
                exc_msg = "executing command '" + n + "': unknown exception";
            }
        }
    }
    if (!exc_msg.empty()) throw std::runtime_error(exc_msg);
}

void Camera::load_config_file(rust::Str path) {
    std::string p(path);
    Pylon::CFeaturePersistence::Load(p.c_str(), &m_impl->camera->GetNodeMap());
}

void Camera::save_config_file(rust::Str path) {
    std::string p(path);
    Pylon::CFeaturePersistence::Save(p.c_str(), &m_impl->camera->GetNodeMap());
}

std::unique_ptr<DeviceInfo> Camera::device_info() const {
    return std::make_unique<DeviceInfo>(
        std::make_unique<DeviceInfo::Impl>(m_impl->camera->GetDeviceInfo()));
}

// ── GrabResult ───────────────────────────────────────────────────────────────

GrabResult::GrabResult(std::unique_ptr<Impl> impl) : m_impl(std::move(impl)) {}
GrabResult::~GrabResult() = default;

bool GrabResult::succeeded() const {
    return m_impl->ptr->GrabSucceeded();
}

rust::String GrabResult::error_description() const {
    return rust::String(m_impl->ptr->GetErrorDescription().c_str());
}

uint32_t GrabResult::width() const {
    return static_cast<uint32_t>(m_impl->ptr->GetWidth());
}

uint32_t GrabResult::height() const {
    return static_cast<uint32_t>(m_impl->ptr->GetHeight());
}

size_t GrabResult::camera_index() const {
    return static_cast<size_t>(m_impl->ptr->GetCameraContext());
}

rust::Slice<const uint8_t> GrabResult::image_data() const {
    const auto* buf = static_cast<const uint8_t*>(m_impl->ptr->GetBuffer());
    size_t size = m_impl->ptr->GetImageSize();
    return rust::Slice<const uint8_t>(buf, size);
}

uint64_t GrabResult::timestamp_ns() const {
    return m_impl->timestamp_ns;
}

rust::String GrabResult::pixel_format() const {
    return rust::String(
        Pylon::CPixelTypeMapper::GetNameByPixelType(m_impl->ptr->GetPixelType()));
}

} // namespace rfp
