// Picture effects and colour. These are taste, not quality: no preset writes them, and changing one keeps the preset.
class ModernMenuEffects extends ModernMenuPage;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddLevelSlider(0, "ModernGodRays", 0, 10, 5);
	AddLevelSlider(1, "ModernVignette", 0, 10, 2);
	AddLevelSlider(2, "ModernChromaticAberration", 0, 10, 0);
	AddLevelSlider(3, "ModernSaturation", 0, 10, 5);
	AddLevelSlider(4, "ModernContrast", 0, 10, 5);
	AddLevelSlider(5, "ModernExposure", 0, 10, 5);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Post Processing"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="God rays"
	Labels(1)="Vignette"
	Labels(2)="Chromatic aberration"
	Labels(3)="Saturation"
	Labels(4)="Contrast"
	Labels(5)="Exposure"

	Helps(0)="Shafts of sunlight, blocked by whatever stands in front of the sun."
	Helps(1)="Darkens the corners of the screen."
	Helps(2)="Colours split slightly towards the screen edges, like a camera lens."
	Helps(3)="How vivid the colours are; 5 is unchanged."
	Helps(4)="The gap between dark and bright; 5 is unchanged."
	Helps(5)="How bright the whole picture is; 5 is unchanged."
}
