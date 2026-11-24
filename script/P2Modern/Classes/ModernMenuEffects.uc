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
	TitleText="Picture Effects"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Sun rays"
	Labels(1)="Vignette"
	Labels(2)="Colour fringe"
	Labels(3)="Colour strength"
	Labels(4)="Contrast"
	Labels(5)="Exposure"

	Helps(0)="Shafts of light around the sun, cut by what stands in front of it. 0 turns them off."
	Helps(1)="Darker corners of the picture. 0 turns it off."
	Helps(2)="Colours split a little towards the edges of the picture, as through a lens. 0 turns it off."
	Helps(3)="How strong the colours are. 5 leaves them as they are."
	Helps(4)="The gap between dark and bright. 5 leaves it as it is."
	Helps(5)="How bright the whole picture is. 5 leaves it as it is."
}
