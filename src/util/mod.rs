// PATHS
pub fn get_paths(recording_id: &str) -> (String, String, String, String) {
    let opus_path = format!(
        "{}/{}.opus",
        std::env::var("GETREC_TMP_PATH").unwrap(),
        recording_id
    );
    let h264_path = format!(
        "{}/{}.h264",
        std::env::var("GETREC_TMP_PATH").unwrap(),
        recording_id
    );
    let mp4_path = format!(
        "{}/{}.mp4",
        std::env::var("GETREC_TMP_PATH").unwrap(),
        recording_id
    );
    let r2_path = format!("{}.mp4", &recording_id);
    (opus_path, h264_path, mp4_path, r2_path)
}

// NETWORK
use std::net::IpAddr;
use systemstat::{Platform, System};

pub fn select_host_address() -> IpAddr {
    let system = System::new();
    let networks = system.networks().unwrap();

    for net in networks.values() {
        for n in &net.addrs {
            if let systemstat::IpAddr::V4(v) = n.addr {
                if !v.is_loopback() && !v.is_link_local() && !v.is_broadcast() {
                    // 192.168.64.1 is docker’s bridge on the desktop
                    if v.to_string() != "192.168.64.1" {
                        return IpAddr::V4(v);
                    }
                }
            }
        }
    }

    panic!("Found no usable network interface");
}

// H264
const NALU_TTYPE_STAP_A: u32 = 24;
const NALU_TTYPE_SPS: u32 = 7;
const NALU_TYPE_BITMASK: u32 = 0x1F;

pub fn get_is_key_frame(data: &[u8]) -> bool {
    if data.len() < 4 {
        false
    } else {
        let word = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let nalu_type = (word >> 24) & NALU_TYPE_BITMASK;
        (nalu_type == NALU_TTYPE_STAP_A && (word & NALU_TYPE_BITMASK) == NALU_TTYPE_SPS)
            || (nalu_type == NALU_TTYPE_SPS)
    }
}

// OPUS
use std::io::BufWriter;
use std::io::Write;

use byteorder::{LittleEndian, WriteBytesExt};

pub const PAGE_HEADER_TYPE_CONTINUATION_OF_STREAM: u8 = 0x00;
const PAGE_HEADER_TYPE_BEGINNING_OF_STREAM: u8 = 0x02;
const DEFAULT_PRE_SKIP: u16 = 3840; // 3840 recommended in the RFC
const ID_PAGE_SIGNATURE: &[u8] = b"OpusHead";
const COMMENT_PAGE_SIGNATURE: &[u8] = b"OpusTags";
const PAGE_HEADER_SIGNATURE: &[u8] = b"OggS";
const PAGE_HEADER_SIZE: usize = 27;
const CHECKSUM_TABLE: [u32; 256] = [
    0, 79764919, 159529838, 222504665, 319059676, 398814059, 445009330, 507990021, 638119352,
    583659535, 797628118, 726387553, 890018660, 835552979, 1015980042, 944750013, 1276238704,
    1221641927, 1167319070, 1095957929, 1595256236, 1540665371, 1452775106, 1381403509, 1780037320,
    1859660671, 1671105958, 1733955601, 2031960084, 2111593891, 1889500026, 1952343757, 2552477408,
    2632100695, 2443283854, 2506133561, 2334638140, 2414271883, 2191915858, 2254759653, 3190512472,
    3135915759, 3081330742, 3009969537, 2905550212, 2850959411, 2762807018, 2691435357, 3560074640,
    3505614887, 3719321342, 3648080713, 3342211916, 3287746299, 3467911202, 3396681109, 4063920168,
    4143685023, 4223187782, 4286162673, 3779000052, 3858754371, 3904687514, 3967668269, 881225847,
    809987520, 1023691545, 969234094, 662832811, 591600412, 771767749, 717299826, 311336399,
    374308984, 453813921, 533576470, 25881363, 88864420, 134795389, 214552010, 2023205639,
    2086057648, 1897238633, 1976864222, 1804852699, 1867694188, 1645340341, 1724971778, 1587496639,
    1516133128, 1461550545, 1406951526, 1302016099, 1230646740, 1142491917, 1087903418, 2896545431,
    2825181984, 2770861561, 2716262478, 3215044683, 3143675388, 3055782693, 3001194130, 2326604591,
    2389456536, 2200899649, 2280525302, 2578013683, 2640855108, 2418763421, 2498394922, 3769900519,
    3832873040, 3912640137, 3992402750, 4088425275, 4151408268, 4197601365, 4277358050, 3334271071,
    3263032808, 3476998961, 3422541446, 3585640067, 3514407732, 3694837229, 3640369242, 1762451694,
    1842216281, 1619975040, 1682949687, 2047383090, 2127137669, 1938468188, 2001449195, 1325665622,
    1271206113, 1183200824, 1111960463, 1543535498, 1489069629, 1434599652, 1363369299, 622672798,
    568075817, 748617968, 677256519, 907627842, 853037301, 1067152940, 995781531, 51762726,
    131386257, 177728840, 240578815, 269590778, 349224269, 429104020, 491947555, 4046411278,
    4126034873, 4172115296, 4234965207, 3794477266, 3874110821, 3953728444, 4016571915, 3609705398,
    3555108353, 3735388376, 3664026991, 3290680682, 3236090077, 3449943556, 3378572211, 3174993278,
    3120533705, 3032266256, 2961025959, 2923101090, 2868635157, 2813903052, 2742672763, 2604032198,
    2683796849, 2461293480, 2524268063, 2284983834, 2364738477, 2175806836, 2238787779, 1569362073,
    1498123566, 1409854455, 1355396672, 1317987909, 1246755826, 1192025387, 1137557660, 2072149281,
    2135122070, 1912620623, 1992383480, 1753615357, 1816598090, 1627664531, 1707420964, 295390185,
    358241886, 404320391, 483945776, 43990325, 106832002, 186451547, 266083308, 932423249,
    861060070, 1041341759, 986742920, 613929101, 542559546, 756411363, 701822548, 3316196985,
    3244833742, 3425377559, 3370778784, 3601682597, 3530312978, 3744426955, 3689838204, 3819031489,
    3881883254, 3928223919, 4007849240, 4037393693, 4100235434, 4180117107, 4259748804, 2310601993,
    2373574846, 2151335527, 2231098320, 2596047829, 2659030626, 2470359227, 2550115596, 2947551409,
    2876312838, 2788305887, 2733848168, 3165939309, 3094707162, 3040238851, 2985771188,
];

pub fn get_opus_id_page(page_index: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(19);
    {
        let mut payload_writer = BufWriter::new(&mut payload);
        // Magic Signature 'OpusHead'
        let _ = payload_writer.write_all(ID_PAGE_SIGNATURE);
        // Version //8
        let _ = payload_writer.write_u8(1);
        // Channel count //9
        let _ = payload_writer.write_u8(2);
        // pre-skip //10-11
        let _ = payload_writer.write_u16::<LittleEndian>(DEFAULT_PRE_SKIP);
        // original sample rate, any valid sample e.g 48000, //12-15
        let _ = payload_writer.write_u32::<LittleEndian>(48000);
        // output gain // 16-17
        let _ = payload_writer.write_u16::<LittleEndian>(0);
        // channel map 0 = one stream: mono or stereo, //18
        let _ = payload_writer.write_u8(0);
    }
    let page = get_opus_page(
        &payload,
        PAGE_HEADER_TYPE_BEGINNING_OF_STREAM,
        0,
        page_index,
    );
    page
}

pub fn get_opus_comment_page(page_index: u32) -> Vec<u8> {
    let mut payload = Vec::with_capacity(22);
    {
        let mut payload_writer = BufWriter::new(&mut payload);
        // Magic Signature 'OpusTags' //0-7
        let _ = payload_writer.write_all(COMMENT_PAGE_SIGNATURE);
        // Vendor Length //8-11
        let _ = payload_writer.write_u32::<LittleEndian>(6);
        // Vendor name //12-20
        let _ = payload_writer.write_all(b"GetRec");
        // User Comment List Length //21-24
        let _ = payload_writer.write_u32::<LittleEndian>(0);
    }
    let page = get_opus_page(
        &payload,
        PAGE_HEADER_TYPE_CONTINUATION_OF_STREAM,
        0,
        page_index,
    );
    page
}

pub fn get_opus_page(
    payload: &Vec<u8>,
    header_type: u8,
    granule_position: u64,
    page_index: u32,
) -> Vec<u8> {
    let n_segments = (payload.len() + 255 - 1) / 255;

    // AUDIO PAYLOAD
    let mut page = Vec::with_capacity(PAGE_HEADER_SIZE + n_segments + payload.len());
    {
        let mut page_writer = BufWriter::new(&mut page);

        // page header
        // page headers starts with 'OggS'//0-3
        let _ = page_writer.write_all(PAGE_HEADER_SIGNATURE);
        // Version//4
        let _ = page_writer.write_u8(0);
        // 1 = continuation, 2 = beginning of stream, 4 = end of stream//5
        let _ = page_writer.write_u8(header_type);
        // granule position //6-13
        let _ = page_writer.write_u64::<LittleEndian>(granule_position);
        // Bitstream serial number//14-17
        let _ = page_writer.write_u32::<LittleEndian>(42069);
        // Page sequence number//18-21
        let _ = page_writer.write_u32::<LittleEndian>(page_index);
        //Checksum reserve //22-25
        let _ = page_writer.write_u32::<LittleEndian>(0);
        // Number of segments in page //26
        let _ = page_writer.write_u8(n_segments as u8);

        // payload lengths (segment lengths)
        // first (n_segments -1) values will always be 255
        for _ in 0..n_segments - 1 {
            let _ = page_writer.write_u8(255);
        }
        // last value will be the remainder, accommodates for payload of size 255
        let _ = page_writer.write_u8((payload.len() - (n_segments - 1) * 255) as u8);
        // audio sample data
        let _ = page_writer.write_all(payload);
    }
    let mut checksum: u32 = 0;
    for v in &page {
        checksum = (checksum << 8) ^ CHECKSUM_TABLE[(((checksum >> 24) as u8) ^ (*v)) as usize];
    }
    page[22..26].copy_from_slice(&checksum.to_le_bytes()); // Checksum - generating for page data and inserting at 22th position into 32 bits
    page
}
