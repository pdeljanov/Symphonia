use symphonia_codec_dca::{DcaDecoder, DcaReader};
use symphonia_core::codecs::audio::{
    AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_DCA,
};
use symphonia_core::errors;
use symphonia_core::formats::probe::ProbeableFormat;
use symphonia_core::io::MediaSourceStream;

fn test_decode(data: Vec<u8>) -> symphonia_core::errors::Result<()> {
    let data = std::io::Cursor::new(data);

    let mss = MediaSourceStream::new(Box::new(data), Default::default());

    let mut reader = DcaReader::try_probe_new(mss, Default::default())?;

    let mut decoder = DcaDecoder::try_new(
        AudioCodecParameters::new().for_codec(CODEC_ID_DCA),
        &AudioDecoderOptions::default(),
    )?;

    loop {
        match reader.next_packet()? {
            Some(packet) => {
                decoder.decode(&packet)?;
            }
            None => break,
        };
    }

    Ok(())
}

#[test]
fn test_minimal_dca() {
    // Generate a minimal DCA frame.
    // Sync: 0x7F FE 80 01
    // Next 8 bytes:
    // Bit 32: Frame type (0)
    // Bits 33-37: Deficit samples (0)
    // Bit 38: CPF (0)
    // Bits 39-45: NBLKS (0x3F = 63 -> 64 blocks)
    // Bits 46-59: FSIZE (0x1FF = 511 -> 512 bytes)
    // Bits 60-65: AMODE (0x1 = 1 channel)
    // Bits 66-69: SFREQ (0x8 = 44100 Hz)
    // ...
    
    let mut file = vec![0x7f, 0xfe, 0x80, 0x01];
    
    // NBLKS=0x3F, FSIZE=0x1FF, AMODE=0x1, SFREQ=0x8
    // Bits: 0 (type) | 00000 (deficit) | 0 (cpf) | 0111111 (nblks) | 00000111111111 (fsize) | 000001 (amode) | 1000 (sfreq)
    // 00000001 11111000 00111111 11100000 11000...
    // Let's just manually craft some bytes that might work with my simple parser.
    // My parser does:
    // bs.ignore_bits(7); // type, deficit, cpf
    // nblks = bs.read_bits(7);
    // fsize = bs.read_bits(14);
    // amode = bs.read_bits(6);
    // sfreq = bs.read_bits(4);
    
    // Byte 0: 0 (type) 00000 (deficit) 0 (cpf) 0 (nblks bit 0) -> 0x00
    // Byte 1: 111111 (nblks remaining) 00 (fsize bits 0-1) -> 0xFC
    // Byte 2: 00011111 (fsize bits 2-9) -> 0x1F
    // Byte 3: 1111 (fsize bits 10-13) 0000 (amode bits 0-3) -> 0xF0
    // Byte 4: 01 (amode bits 4-5) 1000 (sfreq) 00 (padding) -> 0x60
    
    file.extend_from_slice(&[0x00, 0xFC, 0x1F, 0xF0, 0x60, 0x00, 0x00, 0x00]);
    
    // Padding to reach fsize (512)
    file.resize(512, 0);

    let err = test_decode(file).unwrap_err();

    // Since we haven't implemented the decoder, it should return Unsupported.
    assert!(matches!(err, errors::Error::Unsupported(_)));
}
